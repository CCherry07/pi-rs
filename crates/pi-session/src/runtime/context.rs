//! The one write seam for projected session context and its public snapshot.
//!
//! Callers hold Session operation admission and run policy hooks before entering
//! here. The journal owns persistence; this module owns activation and publication.
use super::{AgentSession, SESSION_CLOSED, SESSION_OPEN, SESSION_SHUTTING_DOWN};
use crate::{
    CustomEntry, LaneRecordEntry, MAIN_LANE, NewLaneRecord, SessionUsage, UsageAttribution,
    UsageRecord, next_unique_id,
};
use pi_core::{CustomMessage, Usage};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pi_core::{AssistantMessage, Message, StopReason};
use pi_runtime::{PiRuntime, RuntimeRestoreState};

use super::configuration::clamp_thinking_for_model;
use crate::runtime::events::AgentSessionEventHub;
use crate::{
    BranchSummaryEntry, SessionContext, SessionContextBuildOptions, SessionEntry, SessionError,
    SessionModel, SessionRecord, SessionStore,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ContextActivation {
    /// Restore both configuration and projected messages from the selected branch.
    Restore,
    /// Install projected messages while preserving run prompt, selection and queues.
    KeepConfiguration,
    /// The Agent is executing. Apply this projection between turns, then reconcile
    /// its owned transcript after that run returns.
    AfterRun,
}

pub(super) struct ContextCommit {
    pub(super) record: SessionRecord,
    pub(super) context: SessionContext,
}

pub(super) struct ContextUpdates {
    runtime: PiRuntime,
    store: SessionStore,
    options: SessionContextBuildOptions,
    events: Arc<AgentSessionEventHub>,
    pending: AtomicBool,
}

impl ContextUpdates {
    pub(super) fn new(
        runtime: PiRuntime,
        store: SessionStore,
        options: SessionContextBuildOptions,
        events: Arc<AgentSessionEventHub>,
    ) -> Self {
        Self {
            runtime,
            store,
            options,
            events,
            pending: AtomicBool::new(false),
        }
    }

    pub(super) fn options(&self) -> &SessionContextBuildOptions {
        &self.options
    }

    pub(super) fn current(&self) -> Result<SessionContext, SessionError> {
        self.store
            .context_with_options(&self.options)
            .map_err(Into::into)
    }

    /// A turn controller consumes the pending projection without changing the
    /// Agent's owned transcript while that Agent is still running.
    pub(super) fn pending_context(&self) -> Result<Option<SessionContext>, SessionError> {
        self.pending
            .load(Ordering::Acquire)
            .then(|| self.current())
            .transpose()
    }

    pub(super) fn append(
        &self,
        entry: SessionEntry,
        activation: ContextActivation,
    ) -> Result<ContextCommit, SessionError> {
        let record = self.store.append_session_record(entry)?;
        let context = self.current()?;
        self.activate_and_publish(&context, std::slice::from_ref(&record), activation)?;
        Ok(ContextCommit { record, context })
    }

    pub(super) fn append_batch(
        &self,
        entries: Vec<SessionEntry>,
        activation: ContextActivation,
    ) -> Result<(), SessionError> {
        let records = self.store.append_batch_records(entries)?;
        if !records.is_empty() {
            self.activate_and_publish(&self.current()?, &records, activation)?;
        }
        Ok(())
    }

    pub(super) fn checkout(
        &self,
        leaf_id: Option<&str>,
        context: &SessionContext,
    ) -> Result<(), SessionError> {
        let previous_leaf = self.store.leaf_id();
        self.store.move_lane(crate::MAIN_LANE, leaf_id)?;
        if let Err(error) = self.activate_and_publish(context, &[], ContextActivation::Restore) {
            let _ = self
                .store
                .move_lane(crate::MAIN_LANE, previous_leaf.as_deref());
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn branch(
        &self,
        leaf_id: Option<&str>,
        summary: BranchSummaryEntry,
        label: Option<String>,
    ) -> Result<SessionRecord, SessionError> {
        self.store.move_lane(crate::MAIN_LANE, leaf_id)?;
        let record = self
            .store
            .append_session_record(SessionEntry::BranchSummary(summary))?;
        if let Some(label) = label {
            self.store.set_label(&record.id, Some(label))?;
        }
        self.activate_and_publish(
            &self.current()?,
            std::slice::from_ref(&record),
            ContextActivation::Restore,
        )?;
        Ok(record)
    }

    pub(super) fn reconcile(&self) -> Result<(), SessionError> {
        if let Some(context) = self.pending_context()? {
            self.activate_and_publish(&context, &[], ContextActivation::KeepConfiguration)?;
        }
        Ok(())
    }

    /// Keep failed attempts durable while excluding them from future provider
    /// context. Later boundary projections must not resurrect a retried failure.
    pub(super) fn omit_failed_assistant(
        &self,
        assistant: &AssistantMessage,
    ) -> Result<(), SessionError> {
        let branch = self.store.branch_entries()?;
        let target = branch.iter().rev().find(|record| matches!(&record.entry,
            SessionEntry::Message(message) if matches!(message.message.as_standard(), Some(Message::Assistant(value))
                if value.timestamp_ms == assistant.timestamp_ms && value.stop_reason == StopReason::Error)));
        if let Some(target) = target {
            self.append(
                SessionEntry::ContextEdit(crate::ContextEditEntry {
                    target_id: target.id.clone(),
                    replacement: None,
                }),
                ContextActivation::KeepConfiguration,
            )?;
        } else {
            self.runtime
                .agent()
                .remove_last_failed_assistant()
                .map_err(|error| SessionError::Runtime(error.to_string()))?;
            self.events.refresh_agent(self.runtime.agent().state());
        }
        Ok(())
    }

    pub(super) fn activate_and_publish(
        &self,
        context: &SessionContext,
        records: &[SessionRecord],
        activation: ContextActivation,
    ) -> Result<(), SessionError> {
        match activation {
            ContextActivation::Restore => restore_runtime_context(&self.runtime, context)?,
            ContextActivation::KeepConfiguration => self
                .runtime
                .agent()
                .replace_context_messages(context.runtime_messages())
                .map_err(|error| SessionError::Runtime(error.to_string()))?,
            ContextActivation::AfterRun => {
                self.pending.store(true, Ordering::Release);
                for record in records {
                    self.events.publish_entry(record.clone());
                }
                return Ok(());
            }
        }
        self.pending.store(false, Ordering::Release);
        let state = self.runtime.agent().state();
        if records.is_empty() {
            self.events.refresh_agent(state);
        } else {
            for record in records {
                self.events
                    .publish_initialized_context(record.clone(), state.clone());
            }
        }
        Ok(())
    }
}

pub(super) fn restore_runtime_context(
    runtime: &PiRuntime,
    context: &SessionContext,
) -> Result<(), SessionError> {
    restore_runtime_context_with_request(
        runtime,
        context,
        crate::InitialModelRequest::default().session(context.model.clone()),
    )
    .map(drop)
}

pub(super) fn restore_runtime_context_with_request(
    runtime: &PiRuntime,
    context: &SessionContext,
    request: crate::InitialModelRequest,
) -> Result<Option<String>, SessionError> {
    let current = runtime.agent().state();
    let selection = request
        .resolve(runtime)
        .map_err(|error| SessionError::Runtime(error.to_string()))?;
    let crate::InitialModelSelection {
        model: SessionModel { provider, model_id },
        fallback_message,
    } = selection;
    let thinking_level = context
        .thinking_level
        .parse()
        .map_err(SessionError::InvalidPayload)?;
    let thinking_level = clamp_thinking_for_model(runtime, &provider, &model_id, thinking_level);
    let registered_tools = runtime
        .tool_specs()
        .into_iter()
        .map(|spec| spec.name)
        .collect::<std::collections::HashSet<_>>();
    let mut seen = std::collections::HashSet::new();
    let active_tools = context
        .active_tool_names
        .clone()
        .unwrap_or(current.active_tools)
        .into_iter()
        .filter(|name| registered_tools.contains(name) && seen.insert(name.clone()))
        .collect();
    runtime.restore_state(RuntimeRestoreState {
        provider_id: provider,
        model_id,
        thinking_level,
        active_tools,
        messages: context.runtime_messages(),
    })?;
    Ok(fallback_message)
}

impl AgentSession {
    pub(crate) fn isolated_fork_point(
        &self,
    ) -> Result<Option<pi_plugin::IsolatedForkPoint>, SessionError> {
        if !self.store.is_materialized() {
            return Ok(None);
        }
        let entries = self.store.branch_entries()?;
        let state = self.runtime.agent().state();
        let request = state
            .is_running
            .then(|| {
                state
                    .messages
                    .iter()
                    .rev()
                    .find(|message| !matches!(message, Message::ToolResult(_)))
            })
            .flatten();
        let entries = crate::history::fork_entries(&entries, request);
        Ok(entries.last().map(|entry| pi_plugin::IsolatedForkPoint {
            parent_session_id: self.store.id().to_string(),
            parent_entry_id: entry.id.clone(),
        }))
    }

    pub(crate) fn isolated_context_seed(
        &self,
        fork_turns: Option<usize>,
    ) -> Result<crate::history::IsolatedContextSeed, SessionError> {
        let fork_point = self.isolated_fork_point()?.ok_or_else(|| {
            SessionError::Runtime(
                "cannot fork an unsaved or empty session; use fresh context".into(),
            )
        })?;
        self.isolated_context_seed_at(&fork_point, fork_turns)
    }

    pub(crate) fn isolated_context_seed_at(
        &self,
        fork_point: &pi_plugin::IsolatedForkPoint,
        fork_turns: Option<usize>,
    ) -> Result<crate::history::IsolatedContextSeed, SessionError> {
        if fork_point.parent_session_id != self.store.id() {
            return Err(SessionError::Runtime(
                "isolated fork point belongs to another parent session".into(),
            ));
        }
        let entries = self
            .store
            .branch_entries_at(Some(&fork_point.parent_entry_id))?;
        let messages =
            crate::build_session_context(&entries, self.context_updates.options()).messages;
        Ok(crate::history::IsolatedContextSeed {
            parent_session_id: self.store.id().to_string(),
            parent_entry_id: Some(fork_point.parent_entry_id.clone()),
            messages: crate::history::retain_recent_turns(messages, fork_turns),
        })
    }

    /// Called only on a prepared fresh session, before session_start or input.
    /// A custom entry keeps inherited messages out of configuration and billing
    /// reducers, and does not trigger first-assistant materialization.
    pub(crate) fn initialize_isolated_context(
        &self,
        seed: crate::history::IsolatedContextSeed,
    ) -> Result<(), SessionError> {
        let estimated = seed
            .messages
            .iter()
            .map(crate::estimate_tokens)
            .sum::<u64>();
        if let Some(window) = self.active_context_window()
            && estimated >= window
        {
            return Err(SessionError::Runtime(format!(
                "fork context estimate ({estimated} tokens) exceeds child context window ({window}); compact the parent or use fresh context"
            )));
        }
        self.context_updates.append(
            SessionEntry::Custom(CustomEntry {
                custom_type: crate::history::ISOLATED_CONTEXT_TYPE.to_string(),
                data: Some(
                    serde_json::to_value(seed)
                        .map_err(|error| SessionError::InvalidPayload(error.to_string()))?,
                ),
            }),
            ContextActivation::KeepConfiguration,
        )?;
        Ok(())
    }

    /// Persists extension state that is deliberately excluded from model
    /// context and publishes the same semantic entry event as other session
    /// mutations.
    pub fn append_custom_entry(
        &self,
        custom_type: impl Into<String>,
        data: Option<serde_json::Value>,
    ) -> Result<String, SessionError> {
        self.append_custom_record(custom_type, data)
            .map(|record| record.id)
    }

    /// Persists extension state and returns its authoritative v4 record.
    ///
    /// Callers that need a durable event cursor should use this form instead
    /// of reconstructing identity from a later snapshot.
    pub fn append_custom_record(
        &self,
        custom_type: impl Into<String>,
        data: Option<serde_json::Value>,
    ) -> Result<SessionRecord, SessionError> {
        self.ensure_open()?;
        // Agent/tool callbacks run inside the prompt operation and need to
        // journal extension state before returning their tool result. The
        // SessionStore owns atomic mutation sequencing, so this one entry type
        // may append while a real agent run holds the outer operation gate.
        // Other busy operations (compaction, replacement, tree navigation)
        // remain excluded because they have no active run.
        let operation = self.activity.try_begin().ok();
        if operation.is_none() && !self.runtime.agent().is_running() {
            return Err(SessionError::Busy);
        }
        self.append_custom_record_from_plugin(custom_type, data)
    }

    /// Plugin contexts are invoked inside the session's owning operation and
    /// may journal extension state before that operation releases its gate.
    /// Context epoch validation is the authorization boundary for this path.
    pub(crate) fn append_custom_entry_from_plugin(
        &self,
        custom_type: impl Into<String>,
        data: Option<serde_json::Value>,
    ) -> Result<String, SessionError> {
        self.append_custom_record_from_plugin(custom_type, data)
            .map(|record| record.id)
    }

    pub(super) fn append_custom_record_from_plugin(
        &self,
        custom_type: impl Into<String>,
        data: Option<serde_json::Value>,
    ) -> Result<SessionRecord, SessionError> {
        match self.lifecycle_state.load(Ordering::Acquire) {
            SESSION_OPEN | SESSION_SHUTTING_DOWN => {}
            SESSION_CLOSED => return Err(SessionError::Closed),
            _ => return Err(SessionError::Busy),
        }
        let entry = SessionEntry::Custom(CustomEntry {
            custom_type: custom_type.into(),
            data,
        });
        let record = self.store.append_session_record(entry)?;
        self.events.publish_entry(record.clone());
        Ok(record)
    }

    /// Adds usage from an auxiliary provider call without inserting anything
    /// into the session's message tree. The JSONL storage serializes this
    /// append with concurrent Agent and lifecycle writes.
    pub fn record_usage(
        &self,
        usage: Usage,
        details: Option<serde_json::Value>,
    ) -> Result<String, SessionError> {
        if !self.usage_recording_open.load(Ordering::Acquire) {
            return Err(SessionError::Closed);
        }
        let id = next_unique_id("usage");
        self.store.append_record(NewLaneRecord {
            id: id.clone(),
            lane: MAIN_LANE.to_string(),
            record: LaneRecordEntry::Usage(UsageRecord {
                usage: SessionUsage::from(&usage),
                attribution: UsageAttribution::Adjustment {
                    run_id: None,
                    entry_id: None,
                    details,
                },
            }),
        })?;
        self.events.publish_usage(usage);
        Ok(id)
    }

    /// Persists a custom agent message without triggering a provider turn.
    /// Rebuilding runtime context keeps the next turn and resumed sessions in
    /// agreement with the JSONL tree.
    pub fn append_custom_message(&self, message: CustomMessage) -> Result<String, SessionError> {
        let _operation = self.try_begin_operation()?;
        let entry = SessionEntry::custom_message(&message);
        let committed = self
            .context_updates
            .append(entry, ContextActivation::Restore)?;
        Ok(committed.record.id)
    }

    pub fn set_label(&self, entry_id: &str, label: Option<String>) -> Result<(), SessionError> {
        let _operation = self.try_begin_operation()?;
        self.store.set_label(entry_id, label).map_err(Into::into)
    }
}
