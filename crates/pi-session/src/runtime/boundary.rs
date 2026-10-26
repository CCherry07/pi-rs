use std::sync::Arc;

use pi_core::Message;
use pi_plugin::{
    AgentActivityOutcome, AgentHook, BoundaryContextPreview, SessionBoundaryDraft, TurnEndEvent,
};

use super::AgentSession;
use super::context::ContextActivation;
use crate::{
    AgentMessage, CompactionEntry, ContextEditEntry, ContextEditReplacement, ProjectedSessionEntry,
    SessionEntry, SessionError, SessionRecord, estimate_session_context_tokens, next_unique_id,
    now_ms, project_session_entries,
};

#[derive(Clone)]
struct BoundaryProjection {
    entries: Arc<Vec<ProjectedSessionEntry>>,
    messages: Arc<Vec<AgentMessage>>,
    llm_messages: Arc<Vec<Message>>,
}

// Entries are immutable and a leaf identifies the entire selected parent chain.
// Keep this cache within one dispatch; drafts and custom projectors bypass it.
type BoundaryPreviewCache = Option<(Option<String>, BoundaryProjection)>;

impl AgentSession {
    /// Append a branch-local content edit without changing the stored original message.
    pub fn edit_context(
        &self,
        target_id: impl Into<String>,
        replacement: Option<ContextEditReplacement>,
    ) -> Result<String, SessionError> {
        let _operation = self.try_begin_operation()?;
        let edit = ContextEditEntry {
            target_id: target_id.into(),
            replacement,
        };
        let path = self.store.branch_entries()?;
        crate::history::validate_context_edit(&edit, &path)?;
        let committed = self.context_updates.append(
            SessionEntry::ContextEdit(edit),
            ContextActivation::KeepConfiguration,
        )?;
        Ok(committed.record.id)
    }

    fn prepare_boundary(
        &self,
        drafts: &[SessionBoundaryDraft],
    ) -> Result<(Vec<SessionRecord>, Vec<SessionEntry>), SessionError> {
        let mut path = self.store.branch_entries()?;
        let mut prepared = Vec::with_capacity(drafts.len());
        for draft in drafts {
            let entry = match draft {
                SessionBoundaryDraft::Invalid(_) => {
                    return Err(SessionError::InvalidEntry(
                        "invalid boundary draft shape".into(),
                    ));
                }
                SessionBoundaryDraft::Custom(value) => SessionEntry::Custom(value.clone()),
                SessionBoundaryDraft::CustomMessage(value) => {
                    SessionEntry::CustomMessage(value.clone())
                }
                SessionBoundaryDraft::ContextEdit(value) => {
                    crate::history::validate_context_edit(value, &path)?;
                    SessionEntry::ContextEdit(value.clone())
                }
                SessionBoundaryDraft::Compaction(value) => {
                    let context =
                        crate::build_session_context(&path, self.context_updates.options());
                    let retained_tail = if let Some(id) = &value.first_kept_entry_id {
                        let start = path
                            .iter()
                            .position(|entry| &entry.id == id)
                            .ok_or_else(|| SessionError::NotFound(id.clone()))?;
                        let retained = path[start..]
                            .iter()
                            .map(|entry| entry.id.as_str())
                            .collect::<std::collections::HashSet<_>>();
                        project_session_entries(&path, self.context_updates.options())
                            .into_iter()
                            .filter(|entry| {
                                retained.contains(entry.source_entry.id.as_str())
                                    && !matches!(
                                        entry.source_entry.entry,
                                        SessionEntry::Compaction(_)
                                    )
                            })
                            .flat_map(|entry| entry.messages)
                            .collect()
                    } else {
                        Vec::new()
                    };
                    let mut compaction = CompactionEntry {
                        retained_entry_ids: Vec::new(),
                        summary: value.summary.clone(),
                        retained_tail,
                        tokens_before: estimate_session_context_tokens(&path, &context.messages)
                            .tokens,
                        details: value.details.clone(),
                        usage: value.usage.clone(),
                    };
                    crate::history::annotate_compaction(
                        &mut compaction,
                        &path,
                        self.context_updates.options(),
                    );
                    SessionEntry::Compaction(compaction)
                }
            };
            let record = SessionRecord {
                id: next_unique_id("boundary-preview"),
                seq: path.last().map_or(1, |entry| entry.seq + 1),
                parent_id: path.last().map(|entry| entry.id.clone()),
                timestamp_ms: now_ms(),
                entry: entry.clone(),
            };
            path.push(record);
            prepared.push(entry);
        }
        Ok((path, prepared))
    }

    fn boundary_preview(
        &self,
        drafts: &[SessionBoundaryDraft],
        hook: AgentHook,
        cache: &mut BoundaryPreviewCache,
    ) -> Result<BoundaryContextPreview, String> {
        let options = self.context_updates.options();
        if options.entry_transforms.is_empty() && options.entry_projectors.is_empty() {
            return self.default_boundary_preview(drafts, hook, cache);
        }
        // Arbitrary extension projection callbacks need not be pure. Preserve
        // both their invocation count and ordering relative to queue reads.
        let (path, _) = self
            .prepare_boundary(drafts)
            .map_err(|error| error.to_string())?;
        let context = crate::build_session_context(&path, self.context_updates.options());
        let llm_messages = context.provider_messages();
        let pending_messages = self.runtime.agent().peek_queued_messages();
        let final_assistant = matches!(llm_messages.last(), Some(Message::Assistant(_)));
        let can_continue = (!llm_messages.is_empty() && !final_assistant)
            || ((hook == AgentHook::TurnEnd || final_assistant)
                && self.runtime.agent().has_queued_messages());
        let context_entries = project_session_entries(&path, self.context_updates.options());
        Ok(BoundaryContextPreview {
            context_entries: Arc::new(context_entries),
            context_messages: Arc::new(context.messages),
            llm_messages: Arc::new(llm_messages),
            pending_messages,
            can_continue,
        })
    }

    fn default_boundary_preview(
        &self,
        drafts: &[SessionBoundaryDraft],
        hook: AgentHook,
        cache: &mut BoundaryPreviewCache,
    ) -> Result<BoundaryContextPreview, String> {
        let cached = if drafts.is_empty() {
            let leaf = self
                .store
                .leaf_id_for_lane(crate::MAIN_LANE)
                .map_err(|error| error.to_string())?;
            cache
                .as_ref()
                .filter(|(cached_leaf, _)| *cached_leaf == leaf)
                .map(|(_, projection)| projection.clone())
        } else {
            None
        };
        let projection = match cached {
            Some(projection) => projection,
            None => {
                let (path, _) = self
                    .prepare_boundary(drafts)
                    .map_err(|error| error.to_string())?;
                let (context, entries) = crate::history::build_session_context_and_entries(
                    &path,
                    self.context_updates.options(),
                );
                let projection = BoundaryProjection {
                    llm_messages: Arc::new(context.provider_messages()),
                    messages: Arc::new(context.messages),
                    entries: Arc::new(entries),
                };
                if drafts.is_empty() {
                    *cache = Some((
                        path.last().map(|entry| entry.id.clone()),
                        projection.clone(),
                    ));
                }
                projection
            }
        };
        let pending_messages = self.runtime.agent().peek_queued_messages();
        let final_assistant = matches!(projection.llm_messages.last(), Some(Message::Assistant(_)));
        let can_continue = (!projection.llm_messages.is_empty() && !final_assistant)
            || ((hook == AgentHook::TurnEnd || final_assistant)
                && self.runtime.agent().has_queued_messages());
        Ok(BoundaryContextPreview {
            context_entries: projection.entries,
            context_messages: projection.messages,
            llm_messages: projection.llm_messages,
            pending_messages,
            can_continue,
        })
    }

    pub(super) async fn run_boundary(
        &self,
        run_id: &pi_core::RunId,
        mut turn: Option<TurnEndEvent>,
        outcome: AgentActivityOutcome,
        signal: &pi_core::AbortSignal,
    ) -> Result<bool, SessionError> {
        let hook = if turn.is_some() {
            AgentHook::TurnEnd
        } else {
            AgentHook::AgentBeforeSettle
        };
        if !self.plugin_driver.has_boundary_hook(hook) {
            return Ok(
                hook == AgentHook::AgentBeforeSettle && self.runtime.agent().has_queued_messages()
            );
        }
        if let Some(turn) = &mut turn {
            let branch = self.store.branch_entries()?;
            turn.message_entry_id = branch.iter().rev().find_map(|entry| match &entry.entry {
                SessionEntry::Message(message)
                    if message.message.as_standard()
                        == Some(&Message::Assistant(Arc::clone(&turn.message))) =>
                {
                    Some(entry.id.clone())
                }
                _ => None,
            });
            if turn.message_entry_id.is_none() {
                self.plugin_driver.report_boundary_error(
                    hook,
                    "turn_end could not resolve the persisted assistant entry ID",
                );
                return Ok(false);
            }
            turn.tool_result_entry_ids = turn
                .tool_results
                .iter()
                .filter_map(|result| {
                    branch.iter().rev().find_map(|entry| match &entry.entry {
                        SessionEntry::Message(message)
                            if message.message.as_standard()
                                == Some(&Message::tool_result(result.clone())) =>
                        {
                            Some(entry.id.clone())
                        }
                        _ => None,
                    })
                })
                .collect();
        }
        let mut preview_cache = None;
        let result = self
            .plugin_driver
            .dispatch_boundary(
                run_id,
                self.runtime.cwd(),
                signal,
                turn,
                outcome,
                |drafts| self.boundary_preview(drafts, hook, &mut preview_cache),
            )
            .await;
        let drafts = result.entries.unwrap_or_default();
        if !drafts.is_empty() {
            // Revalidate against the current journal after callbacks; no partial draft commits.
            let (_, entries) = self.prepare_boundary(&drafts)?;
            let activation = match hook {
                AgentHook::TurnEnd => ContextActivation::AfterRun,
                _ => ContextActivation::KeepConfiguration,
            };
            self.context_updates.append_batch(entries, activation)?;
        }
        let requested = result.continue_run.unwrap_or(false)
            || (hook == AgentHook::AgentBeforeSettle && self.runtime.agent().has_queued_messages());
        let runnable = self
            .boundary_preview(&[], hook, &mut preview_cache)
            .map_err(SessionError::Runtime)?
            .can_continue;
        if requested && !runnable {
            self.plugin_driver.report_boundary_error(
                hook,
                format!(
                    "{} requested continuation without runnable model context",
                    hook.as_str()
                ),
            );
        }
        Ok(runnable && requested && !signal.is_aborted() && !self.activity.is_aborted())
    }
}
