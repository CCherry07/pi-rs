use std::collections::{HashMap, HashSet};

use pi_core::{AssistantMessage, DeferredHandle, Message, StopReason, ToolCall};
use serde::{Deserialize, Serialize};

use crate::{
    CompactionReason, LaneRecord, LaneRecordEntry, OperationIntent, OperationKind,
    ProvisionedEntry, QueueKind, SessionEntry, SessionEntryType, SessionModel, SessionRecord,
    StepKind, UsageAttribution,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordLogCorruptionReason {
    MultipleOpenOperations,
    UnknownOperation,
    RecordAfterFinish,
    NonConsecutiveAttempt,
    InvalidCompactionReason,
    QueueAfterAbort,
    InvalidQueueCancellation,
    InconsistentStep,
    ToolCallMismatch,
    DuplicateToolInvocation,
    ProvisionedEntryMismatch,
    InvalidDeferredHandle,
}

impl RecordLogCorruptionReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MultipleOpenOperations => "multiple_open_operations",
            Self::UnknownOperation => "unknown_operation",
            Self::RecordAfterFinish => "record_after_finish",
            Self::NonConsecutiveAttempt => "non_consecutive_attempt",
            Self::InvalidCompactionReason => "invalid_compaction_reason",
            Self::QueueAfterAbort => "queue_after_abort",
            Self::InvalidQueueCancellation => "invalid_queue_cancellation",
            Self::InconsistentStep => "inconsistent_step",
            Self::ToolCallMismatch => "tool_call_mismatch",
            Self::DuplicateToolInvocation => "duplicate_tool_invocation",
            Self::ProvisionedEntryMismatch => "provisioned_entry_mismatch",
            Self::InvalidDeferredHandle => "invalid_deferred_handle",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct RecordLogCorruption {
    pub reason: RecordLogCorruptionReason,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecordLogSlice {
    pub lane: String,
    pub open_operations: Vec<LaneRecord>,
    pub records: Vec<LaneRecord>,
    /// Operation-owned entries plus entries fetched by provisioned/referenced id.
    pub entries: Vec<SessionRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveLaneConfiguration {
    pub model: SessionModel,
    pub thinking_level: String,
    pub active_tool_names: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalFailureSource {
    Step,
    DeferredFetch,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TerminalFailureState {
    pub entry_id: String,
    pub source: TerminalFailureSource,
    pub message: AssistantMessage,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolBatchCallState {
    pub tool_index: usize,
    pub tool_call: ToolCall,
    pub started: Option<LaneRecord>,
    pub result_exists: bool,
    pub terminate: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolBatchState {
    pub assistant_entry_id: String,
    pub calls: Vec<ToolBatchCallState>,
    pub truncated: bool,
    pub unresolved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneStepState {
    pub kind: StepKind,
    pub attempts: u32,
    pub result_entry_id: String,
    pub compaction_reason: Option<CompactionReason>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewestOwnEntryState {
    pub entry_id: String,
    pub entry_type: SessionEntryType,
    pub role: Option<String>,
    pub stop_reason: Option<StopReason>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OperationTargetState {
    pub result: Option<bool>,
    pub summary: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LaneOperationState {
    pub id: String,
    pub kind: OperationKind,
    pub intent: OperationIntent,
    pub aborting: bool,
    pub step: Option<LaneStepState>,
    pub tool_batch: Option<ToolBatchState>,
    pub missing_initial_messages: Vec<ProvisionedEntry>,
    pub pending_steer: Vec<ProvisionedEntry>,
    pub pending_follow_up: Vec<ProvisionedEntry>,
    pub pending_writes: Vec<ProvisionedEntry>,
    pub deferred: Option<DeferredHandle>,
    pub overflow_recovery_used: bool,
    pub newest_own: Option<NewestOwnEntryState>,
    pub targets: OperationTargetState,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LaneState {
    pub lane: String,
    pub leaf_id: Option<String>,
    pub operation: Option<LaneOperationState>,
    pub pending_next_run: Vec<ProvisionedEntry>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LaneReductionInput {
    pub slice: RecordLogSlice,
    pub leaf_id: Option<String>,
    /// Entries appended by the open operation, oldest first.
    pub own_entries: Vec<SessionRecord>,
    /// Bounded effective-state lookups at the operation anchor or idle leaf.
    pub configuration_entries: Vec<SessionRecord>,
    pub defaults: EffectiveLaneConfiguration,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LaneReductionResult {
    pub lane_state: LaneState,
    pub effective_configuration: EffectiveLaneConfiguration,
    pub terminal_failure: Option<TerminalFailureState>,
}

fn corruption(
    reason: RecordLogCorruptionReason,
    message: impl Into<String>,
) -> RecordLogCorruption {
    RecordLogCorruption {
        reason,
        message: message.into(),
    }
}

fn provisioned_entries_match(entry: &SessionRecord, target: &ProvisionedEntry) -> bool {
    serde_json::to_value(entry.provisioned()).ok() == serde_json::to_value(target).ok()
}

fn validate_exact_provisioned_entry(
    entries_by_id: &HashMap<&str, &SessionRecord>,
    target: &ProvisionedEntry,
) -> Result<(), RecordLogCorruption> {
    if let Some(entry) = entries_by_id.get(target.id.as_str())
        && !provisioned_entries_match(entry, target)
    {
        return Err(corruption(
            RecordLogCorruptionReason::ProvisionedEntryMismatch,
            format!(
                "provisioned entry {} exists with content different from its intent",
                target.id
            ),
        ));
    }
    Ok(())
}

fn validate_result_entry(
    entries_by_id: &HashMap<&str, &SessionRecord>,
    result_entry_id: &str,
    matches: impl FnOnce(&SessionRecord) -> bool,
    description: &str,
) -> Result<(), RecordLogCorruption> {
    if let Some(entry) = entries_by_id.get(result_entry_id)
        && !matches(entry)
    {
        return Err(corruption(
            RecordLogCorruptionReason::ProvisionedEntryMismatch,
            format!(
                "provisioned {description} entry {result_entry_id} exists with different content"
            ),
        ));
    }
    Ok(())
}

fn step_attempt(
    record: &LaneRecord,
) -> Option<(&str, StepKind, u32, &str, Option<CompactionReason>)> {
    let LaneRecordEntry::StepAttempt {
        run_id,
        step,
        attempt,
        result_entry_id,
        compaction_reason,
    } = &record.record
    else {
        return None;
    };
    Some((run_id, *step, *attempt, result_entry_id, *compaction_reason))
}

fn validate_attempt_reason(record: &LaneRecord) -> Result<(), RecordLogCorruption> {
    let Some((_, step, _, _, reason)) = step_attempt(record) else {
        return Ok(());
    };
    let valid = matches!(
        (step, reason),
        (StepKind::Compaction, Some(_)) | (StepKind::Assistant | StepKind::BranchSummary, None)
    );
    if valid {
        Ok(())
    } else {
        Err(corruption(
            RecordLogCorruptionReason::InvalidCompactionReason,
            format!(
                "step attempt {} has an invalid compaction reason",
                record.id
            ),
        ))
    }
}

fn validate_attempt_sequence(
    record: &LaneRecord,
    previous: Option<&LaneRecord>,
    entries_by_id: &HashMap<&str, &SessionRecord>,
) -> Result<(), RecordLogCorruption> {
    let Some((_, step, attempt, result_entry_id, reason)) = step_attempt(record) else {
        return Ok(());
    };
    let previous_attempt = previous.and_then(step_attempt);
    let previous_result =
        previous_attempt.and_then(|(_, _, _, result_id, _)| entries_by_id.get(result_id).copied());
    let continues_series = previous_attempt.is_some_and(|(_, previous_step, _, _, _)| {
        previous_step == step && previous_result.is_none_or(|entry| entry.seq >= record.seq)
    });
    let expected = if continues_series {
        previous_attempt.map_or(1, |(_, _, attempt, _, _)| attempt.saturating_add(1))
    } else {
        1
    };
    if attempt != expected {
        return Err(corruption(
            RecordLogCorruptionReason::NonConsecutiveAttempt,
            format!(
                "{step:?} attempt {} is {attempt}; expected {expected}",
                record.id
            ),
        ));
    }
    if !continues_series || step == StepKind::Assistant {
        return Ok(());
    }
    let Some((_, _, _, previous_result_id, previous_reason)) = previous_attempt else {
        return Ok(());
    };
    if result_entry_id != previous_result_id {
        return Err(corruption(
            RecordLogCorruptionReason::InconsistentStep,
            format!("{step:?} attempts disagree on their result entry id"),
        ));
    }
    if reason != previous_reason {
        return Err(corruption(
            RecordLogCorruptionReason::InconsistentStep,
            format!("{step:?} attempts disagree on their compaction reason"),
        ));
    }
    Ok(())
}

fn validate_attempt_result(
    entries_by_id: &HashMap<&str, &SessionRecord>,
    record: &LaneRecord,
) -> Result<(), RecordLogCorruption> {
    let Some((_, step, _, result_entry_id, _)) = step_attempt(record) else {
        return Ok(());
    };
    match step {
        StepKind::Assistant => validate_result_entry(
            entries_by_id,
            result_entry_id,
            |entry| {
                matches!(
                    &entry.entry,
                    SessionEntry::Message(message)
                        if matches!(message.message.as_standard(), Some(Message::Assistant(_)))
                )
            },
            "assistant result",
        ),
        StepKind::Compaction => validate_result_entry(
            entries_by_id,
            result_entry_id,
            |entry| matches!(entry.entry, SessionEntry::Compaction(_)),
            "compaction result",
        ),
        StepKind::BranchSummary => validate_result_entry(
            entries_by_id,
            result_entry_id,
            |entry| matches!(entry.entry, SessionEntry::BranchSummary(_)),
            "branch-summary result",
        ),
    }
}

fn validate_tool_start(
    record: &LaneRecord,
    entries_by_id: &HashMap<&str, &SessionRecord>,
    invocations: &mut HashSet<(String, usize)>,
) -> Result<(), RecordLogCorruption> {
    let LaneRecordEntry::ToolStarted {
        assistant_entry_id,
        tool_index,
        tool_call_id,
        tool_name,
        result_entry_id,
        ..
    } = &record.record
    else {
        return Ok(());
    };
    if !invocations.insert((assistant_entry_id.clone(), *tool_index)) {
        return Err(corruption(
            RecordLogCorruptionReason::DuplicateToolInvocation,
            format!("tool invocation {assistant_entry_id}:{tool_index} is duplicated"),
        ));
    }

    let Some(assistant_entry) = entries_by_id.get(assistant_entry_id.as_str()) else {
        return Err(corruption(
            RecordLogCorruptionReason::ToolCallMismatch,
            format!(
                "tool start {} does not reference an assistant entry",
                record.id
            ),
        ));
    };
    let SessionEntry::Message(message_entry) = &assistant_entry.entry else {
        return Err(corruption(
            RecordLogCorruptionReason::ToolCallMismatch,
            format!(
                "tool start {} does not reference an assistant entry",
                record.id
            ),
        ));
    };
    let Some(Message::Assistant(assistant)) = message_entry.message.as_standard() else {
        return Err(corruption(
            RecordLogCorruptionReason::ToolCallMismatch,
            format!(
                "tool start {} does not reference an assistant entry",
                record.id
            ),
        ));
    };
    let tool_call = assistant
        .content
        .iter()
        .filter_map(|content| match content {
            pi_core::ContentBlock::ToolCall(call) => Some(call),
            _ => None,
        })
        .nth(*tool_index);
    if !tool_call.is_some_and(|call| call.id.as_str() == tool_call_id && call.name == *tool_name) {
        return Err(corruption(
            RecordLogCorruptionReason::ToolCallMismatch,
            format!(
                "tool start {} does not match its assistant tool-call ordinal",
                record.id
            ),
        ));
    }

    validate_result_entry(
        entries_by_id,
        result_entry_id,
        |entry| {
            let SessionEntry::Message(message_entry) = &entry.entry else {
                return false;
            };
            matches!(
                message_entry.message.as_standard(),
                Some(Message::ToolResult(result))
                    if result.tool_call_id.as_str() == tool_call_id && result.tool_name == *tool_name
            )
        },
        "tool result",
    )
}

fn validate_deferred_handles<'a>(
    entries: impl IntoIterator<Item = &'a SessionRecord>,
) -> Result<(), RecordLogCorruption> {
    for entry in entries {
        let SessionEntry::Message(message_entry) = &entry.entry else {
            continue;
        };
        if matches!(
            message_entry.message.as_standard(),
            Some(Message::Assistant(message))
                if message.stop_reason == StopReason::Deferred && message.deferred.is_none()
        ) {
            return Err(corruption(
                RecordLogCorruptionReason::InvalidDeferredHandle,
                format!(
                    "deferred assistant entry {} does not carry a handle",
                    entry.id
                ),
            ));
        }
    }
    Ok(())
}

fn validate_operation_result(
    entries_by_id: &HashMap<&str, &SessionRecord>,
    deferred_targets: &HashMap<&str, &ProvisionedEntry>,
    record: &LaneRecord,
) -> Result<(), RecordLogCorruption> {
    let LaneRecordEntry::OperationStarted { intent, .. } = &record.record else {
        return Ok(());
    };
    match intent {
        OperationIntent::Run {
            initial_messages, ..
        } => {
            for target in initial_messages {
                validate_exact_provisioned_entry(
                    entries_by_id,
                    deferred_targets
                        .get(target.id.as_str())
                        .copied()
                        .unwrap_or(target),
                )?;
            }
            Ok(())
        }
        OperationIntent::Compaction {
            result_entry_id, ..
        } => validate_result_entry(
            entries_by_id,
            result_entry_id,
            |entry| matches!(entry.entry, SessionEntry::Compaction(_)),
            "manual compaction",
        ),
        OperationIntent::Navigation {
            summary_entry_id, ..
        } => match summary_entry_id {
            Some(summary_entry_id) => validate_result_entry(
                entries_by_id,
                summary_entry_id,
                |entry| matches!(entry.entry, SessionEntry::BranchSummary(_)),
                "navigation summary",
            ),
            None => Ok(()),
        },
    }
}

/// Validates the bounded recovery slice without reading or mutating storage.
pub fn validate_record_log(input: &RecordLogSlice) -> Result<(), RecordLogCorruption> {
    if input.open_operations.len() > 1 {
        return Err(corruption(
            RecordLogCorruptionReason::MultipleOpenOperations,
            format!("lane {} has at least two open operations", input.lane),
        ));
    }

    let entries_by_id = input
        .entries
        .iter()
        .map(|entry| (entry.id.as_str(), entry))
        .collect::<HashMap<_, _>>();
    validate_deferred_handles(entries_by_id.values().copied())?;

    let mut starts = HashMap::<String, &LaneRecord>::new();
    let mut finished_at = HashMap::<String, u64>::new();
    let mut aborted_at = HashMap::<String, u64>::new();
    let mut queue_enqueues = HashMap::<String, &LaneRecord>::new();
    let mut latest_attempt = HashMap::<String, &LaneRecord>::new();
    let mut tool_invocations = HashSet::new();
    let mut records = input.records.iter().collect::<Vec<_>>();
    records.sort_by_key(|record| record.seq);
    let deferred_targets = records
        .iter()
        .filter_map(|record| match &record.record {
            LaneRecordEntry::WriteDeferred { target, .. } => Some((target.id.as_str(), target)),
            _ => None,
        })
        .collect::<HashMap<_, _>>();

    for record in records {
        if matches!(record.record, LaneRecordEntry::OperationStarted { .. }) {
            starts.insert(record.id.clone(), record);
            validate_operation_result(&entries_by_id, &deferred_targets, record)?;
            continue;
        }

        if let Some(run_id) = record.record.run_id() {
            if !starts.contains_key(run_id) {
                return Err(corruption(
                    RecordLogCorruptionReason::UnknownOperation,
                    format!("record {} references unknown operation {run_id}", record.id),
                ));
            }
            if finished_at
                .get(run_id)
                .is_some_and(|finish_seq| record.seq > *finish_seq)
            {
                return Err(corruption(
                    RecordLogCorruptionReason::RecordAfterFinish,
                    format!(
                        "record {} follows the finish of operation {run_id}",
                        record.id
                    ),
                ));
            }
        }

        match &record.record {
            LaneRecordEntry::OperationFinished { run_id, .. } => {
                finished_at.insert(run_id.clone(), record.seq);
            }
            LaneRecordEntry::AbortRequested { run_id } => {
                aborted_at.insert(run_id.clone(), record.seq);
            }
            LaneRecordEntry::StepAttempt { run_id, .. } => {
                validate_attempt_reason(record)?;
                validate_attempt_sequence(
                    record,
                    latest_attempt.get(run_id).copied(),
                    &entries_by_id,
                )?;
                validate_attempt_result(&entries_by_id, record)?;
                latest_attempt.insert(run_id.clone(), record);
            }
            LaneRecordEntry::ToolStarted { .. } => {
                validate_tool_start(record, &entries_by_id, &mut tool_invocations)?;
            }
            LaneRecordEntry::QueueEnqueued {
                queue,
                run_id,
                target,
            } => {
                if *queue != QueueKind::NextRun
                    && run_id
                        .as_ref()
                        .and_then(|id| aborted_at.get(id))
                        .is_some_and(|abort_seq| record.seq > *abort_seq)
                {
                    return Err(corruption(
                        RecordLogCorruptionReason::QueueAfterAbort,
                        format!("{queue:?} item {} was enqueued after abort", target.id),
                    ));
                }
                queue_enqueues.insert(target.id.clone(), record);
                validate_exact_provisioned_entry(
                    &entries_by_id,
                    deferred_targets
                        .get(target.id.as_str())
                        .copied()
                        .unwrap_or(target),
                )?;
            }
            LaneRecordEntry::QueueCancelled { run_id, entry_id } => {
                let enqueue = queue_enqueues.get(entry_id).copied();
                let valid = enqueue.is_some_and(|enqueue| {
                    let LaneRecordEntry::QueueEnqueued {
                        run_id: enqueue_run_id,
                        ..
                    } = &enqueue.record
                    else {
                        return false;
                    };
                    enqueue.seq < record.seq
                        && enqueue_run_id == run_id
                        && !entries_by_id.contains_key(entry_id.as_str())
                });
                if !valid {
                    return Err(corruption(
                        RecordLogCorruptionReason::InvalidQueueCancellation,
                        format!(
                            "queue cancellation {} has no pending matching enqueue",
                            record.id
                        ),
                    ));
                }
            }
            LaneRecordEntry::WriteDeferred { target, .. } => {
                validate_exact_provisioned_entry(&entries_by_id, target)?;
            }
            LaneRecordEntry::Usage(_) => {}
            LaneRecordEntry::OperationStarted { .. } => unreachable!(),
        }
    }
    Ok(())
}

fn derive_effective_configuration(input: &LaneReductionInput) -> EffectiveLaneConfiguration {
    let mut configuration = input.defaults.clone();
    let mut entries_by_id = HashMap::<String, SessionRecord>::new();
    for entry in input.configuration_entries.iter().chain(&input.own_entries) {
        entries_by_id.insert(entry.id.clone(), entry.clone());
    }
    let mut entries = entries_by_id.into_values().collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.seq);
    for entry in entries {
        match entry.entry {
            SessionEntry::ModelChange(change) => {
                configuration.model = SessionModel {
                    provider: change.provider,
                    model_id: change.model_id,
                };
            }
            SessionEntry::ThinkingLevelChange(change) => {
                configuration.thinking_level = change.thinking_level;
            }
            SessionEntry::ActiveToolsChange(change) => {
                configuration.active_tool_names = change.active_tool_names;
            }
            SessionEntry::Message(message) => {
                if let Some(Message::Assistant(assistant)) = message.message.as_standard() {
                    configuration.model = SessionModel {
                        provider: assistant.provider.clone(),
                        model_id: assistant.model.clone(),
                    };
                }
            }
            SessionEntry::Compaction(_)
            | SessionEntry::BranchSummary(_)
            | SessionEntry::CustomMessage(_)
            | SessionEntry::Custom(_) => {}
        }
    }
    configuration
}

fn derive_newest_own(entry: Option<&SessionRecord>) -> Option<NewestOwnEntryState> {
    let entry = entry?;
    let entry_type = entry.entry.entry_type();
    let SessionEntry::Message(message_entry) = &entry.entry else {
        return Some(NewestOwnEntryState {
            entry_id: entry.id.clone(),
            entry_type,
            role: None,
            stop_reason: None,
        });
    };
    let role = Some(message_entry.message.role().to_string());
    let stop_reason = match message_entry.message.as_standard() {
        Some(Message::Assistant(message)) => Some(message.stop_reason),
        _ => None,
    };
    Some(NewestOwnEntryState {
        entry_id: entry.id.clone(),
        entry_type,
        role,
        stop_reason,
    })
}

fn derive_tool_batch(
    operation_id: &str,
    records: &[&LaneRecord],
    own_entries: &[SessionRecord],
    entries_by_id: &HashMap<&str, &SessionRecord>,
    deferred_write_ids: &HashSet<&str>,
) -> Option<ToolBatchState> {
    let (assistant_entry, assistant) = own_entries.iter().rev().find_map(|entry| {
        let SessionEntry::Message(message_entry) = &entry.entry else {
            return None;
        };
        let Some(Message::Assistant(assistant)) = message_entry.message.as_standard() else {
            return None;
        };
        assistant
            .content
            .iter()
            .any(|content| matches!(content, pi_core::ContentBlock::ToolCall(_)))
            .then_some((entry, assistant))
    })?;
    let tool_calls = assistant
        .content
        .iter()
        .filter_map(|content| match content {
            pi_core::ContentBlock::ToolCall(call) => Some(call.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut starts = HashMap::<usize, &LaneRecord>::new();
    for record in records {
        if let LaneRecordEntry::ToolStarted {
            run_id,
            assistant_entry_id,
            tool_index,
            ..
        } = &record.record
            && run_id == operation_id
            && assistant_entry_id == &assistant_entry.id
        {
            starts.insert(*tool_index, record);
        }
    }

    let calls = tool_calls
        .into_iter()
        .enumerate()
        .map(|(tool_index, tool_call)| {
            let started = starts.get(&tool_index).copied();
            let started_result = started.and_then(|record| {
                let LaneRecordEntry::ToolStarted {
                    result_entry_id, ..
                } = &record.record
                else {
                    return None;
                };
                entries_by_id.get(result_entry_id.as_str()).copied()
            });
            let blocked_result = own_entries.iter().find(|entry| {
                if entry.seq <= assistant_entry.seq
                    || deferred_write_ids.contains(entry.id.as_str())
                {
                    return false;
                }
                let SessionEntry::Message(message_entry) = &entry.entry else {
                    return false;
                };
                matches!(
                    message_entry.message.as_standard(),
                    Some(Message::ToolResult(result)) if result.tool_call_id == tool_call.id
                )
            });
            let result = started_result.or(blocked_result);
            let terminate = result.and_then(|entry| match &entry.entry {
                SessionEntry::Message(message) if message.terminate => Some(true),
                _ => None,
            });
            ToolBatchCallState {
                tool_index,
                tool_call,
                started: started.cloned(),
                result_exists: result.is_some(),
                terminate,
            }
        })
        .collect::<Vec<_>>();
    Some(ToolBatchState {
        assistant_entry_id: assistant_entry.id.clone(),
        truncated: assistant.stop_reason == StopReason::Length,
        unresolved: calls.iter().any(|call| !call.result_exists),
        calls,
    })
}

/// Purely reconstructs one lane's orchestration state from bounded inputs.
pub fn reduce_lane_state(
    input: &LaneReductionInput,
) -> Result<LaneReductionResult, RecordLogCorruption> {
    validate_record_log(&input.slice)?;

    let mut records = input.slice.records.iter().collect::<Vec<_>>();
    records.sort_by_key(|record| record.seq);
    let mut own_entries = input.own_entries.clone();
    own_entries.sort_by_key(|entry| entry.seq);
    let mut entries_by_id = HashMap::<&str, &SessionRecord>::new();
    for entry in &input.slice.entries {
        entries_by_id.insert(entry.id.as_str(), entry);
    }
    for entry in &own_entries {
        entries_by_id.insert(entry.id.as_str(), entry);
    }

    let cancelled_queue_ids = records
        .iter()
        .filter_map(|record| match &record.record {
            LaneRecordEntry::QueueCancelled { entry_id, .. } => Some(entry_id.as_str()),
            _ => None,
        })
        .collect::<HashSet<_>>();
    let pending_queue_records = records
        .iter()
        .copied()
        .filter(|record| match &record.record {
            LaneRecordEntry::QueueEnqueued { target, .. } => {
                !entries_by_id.contains_key(target.id.as_str())
                    && !cancelled_queue_ids.contains(target.id.as_str())
            }
            _ => false,
        })
        .collect::<Vec<_>>();

    let started = input.slice.open_operations.first();
    let captured_initial_message_ids = started
        .and_then(|record| match &record.record {
            LaneRecordEntry::OperationStarted {
                intent:
                    OperationIntent::Run {
                        initial_messages, ..
                    },
                ..
            } => Some(
                initial_messages
                    .iter()
                    .map(|target| target.id.as_str())
                    .collect::<HashSet<_>>(),
            ),
            _ => None,
        })
        .unwrap_or_default();
    let pending_next_run = pending_queue_records
        .iter()
        .filter_map(|record| match &record.record {
            LaneRecordEntry::QueueEnqueued {
                queue: QueueKind::NextRun,
                target,
                ..
            } if !captured_initial_message_ids.contains(target.id.as_str()) => Some(target.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let effective_configuration = derive_effective_configuration(input);

    let Some(started) = started else {
        return Ok(LaneReductionResult {
            lane_state: LaneState {
                lane: input.slice.lane.clone(),
                leaf_id: input.leaf_id.clone(),
                operation: None,
                pending_next_run,
            },
            effective_configuration,
            terminal_failure: None,
        });
    };
    let LaneRecordEntry::OperationStarted { intent, .. } = &started.record else {
        return Err(corruption(
            RecordLogCorruptionReason::UnknownOperation,
            "open operation projection does not contain an operation start",
        ));
    };

    let operation_records = records
        .iter()
        .copied()
        .filter(|record| {
            matches!(record.record, LaneRecordEntry::OperationStarted { .. })
                .then_some(record.id.as_str())
                .or_else(|| record.record.run_id())
                == Some(started.id.as_str())
        })
        .collect::<Vec<_>>();
    let aborting = operation_records
        .iter()
        .any(|record| matches!(record.record, LaneRecordEntry::AbortRequested { .. }));
    let pending_for_queue = |queue: QueueKind| {
        if aborting {
            return Vec::new();
        }
        pending_queue_records
            .iter()
            .filter_map(|record| match &record.record {
                LaneRecordEntry::QueueEnqueued {
                    queue: candidate,
                    run_id: Some(run_id),
                    target,
                } if *candidate == queue && run_id == &started.id => Some(target.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let pending_steer = pending_for_queue(QueueKind::Steer);
    let pending_follow_up = pending_for_queue(QueueKind::FollowUp);
    let pending_writes = operation_records
        .iter()
        .filter_map(|record| match &record.record {
            LaneRecordEntry::WriteDeferred { target, .. }
                if !entries_by_id.contains_key(target.id.as_str()) =>
            {
                Some(target.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let missing_initial_messages = match intent {
        OperationIntent::Run {
            initial_messages, ..
        } => initial_messages
            .iter()
            .filter(|target| !entries_by_id.contains_key(target.id.as_str()))
            .cloned()
            .collect(),
        OperationIntent::Compaction { .. } | OperationIntent::Navigation { .. } => Vec::new(),
    };

    let newest_attempt = operation_records
        .iter()
        .rev()
        .find(|record| matches!(record.record, LaneRecordEntry::StepAttempt { .. }))
        .copied();
    let step = newest_attempt.and_then(|record| {
        let (_, kind, attempts, result_entry_id, compaction_reason) = step_attempt(record)?;
        (!entries_by_id.contains_key(result_entry_id)).then(|| LaneStepState {
            kind,
            attempts,
            result_entry_id: result_entry_id.to_string(),
            compaction_reason: (kind == StepKind::Compaction)
                .then_some(compaction_reason)
                .flatten(),
        })
    });

    let mut consumed_input_ids = HashSet::<&str>::new();
    if let OperationIntent::Run {
        initial_messages, ..
    } = intent
    {
        consumed_input_ids.extend(initial_messages.iter().map(|target| target.id.as_str()));
    }
    for record in &operation_records {
        if let LaneRecordEntry::QueueEnqueued { queue, target, .. } = &record.record
            && *queue != QueueKind::NextRun
        {
            consumed_input_ids.insert(target.id.as_str());
        }
    }
    let newest_consumed_input_sequence = consumed_input_ids
        .iter()
        .filter_map(|id| entries_by_id.get(id).copied())
        .filter(|entry| matches!(entry.entry, SessionEntry::Message(_)))
        .map(|entry| entry.seq)
        .max();
    let overflow_recovery_used = operation_records.iter().any(|record| {
        matches!(
            &record.record,
            LaneRecordEntry::StepAttempt {
                step: StepKind::Compaction,
                compaction_reason: Some(CompactionReason::Overflow),
                ..
            }
        ) && newest_consumed_input_sequence.is_none_or(|sequence| record.seq > sequence)
    });

    let newest_own_entry = own_entries.last();
    let newest_own = derive_newest_own(newest_own_entry);
    let deferred = newest_own_entry.and_then(|entry| {
        let SessionEntry::Message(message_entry) = &entry.entry else {
            return None;
        };
        match message_entry.message.as_standard() {
            Some(Message::Assistant(message)) if message.stop_reason == StopReason::Deferred => {
                message.deferred.clone()
            }
            _ => None,
        }
    });
    let targets = match intent {
        OperationIntent::Compaction {
            result_entry_id, ..
        } => OperationTargetState {
            result: Some(entries_by_id.contains_key(result_entry_id.as_str())),
            summary: None,
        },
        OperationIntent::Navigation {
            summary_entry_id: Some(summary_entry_id),
            ..
        } => OperationTargetState {
            result: None,
            summary: Some(entries_by_id.contains_key(summary_entry_id.as_str())),
        },
        OperationIntent::Run { .. }
        | OperationIntent::Navigation {
            summary_entry_id: None,
            ..
        } => OperationTargetState::default(),
    };

    let deferred_write_ids = operation_records
        .iter()
        .filter_map(|record| match &record.record {
            LaneRecordEntry::WriteDeferred { target, .. } => Some(target.id.as_str()),
            _ => None,
        })
        .collect::<HashSet<_>>();
    let terminal_failure = newest_own_entry.and_then(|entry| {
        let SessionEntry::Message(message_entry) = &entry.entry else {
            return None;
        };
        let Some(Message::Assistant(message)) = message_entry.message.as_standard() else {
            return None;
        };
        if message.stop_reason != StopReason::Error
            || deferred_write_ids.contains(entry.id.as_str())
        {
            return None;
        }
        let produced_by_step = operation_records.iter().any(|record| {
            matches!(
                &record.record,
                LaneRecordEntry::StepAttempt { result_entry_id, .. } if result_entry_id == &entry.id
            )
        });
        let produced_by_usage = operation_records.iter().any(|record| {
            matches!(
                &record.record,
                LaneRecordEntry::Usage(usage)
                    if matches!(
                        &usage.attribution,
                        UsageAttribution::DeferredFetch { entry_id, .. } if entry_id == &entry.id
                    )
            )
        });
        let produced_after_deferred = own_entries
            .get(own_entries.len().saturating_sub(2))
            .is_some_and(|previous| {
                let SessionEntry::Message(previous_message) = &previous.entry else {
                    return false;
                };
                matches!(
                    previous_message.message.as_standard(),
                    Some(Message::Assistant(previous))
                        if previous.stop_reason == StopReason::Deferred
                )
            });
        (produced_by_step || produced_by_usage || produced_after_deferred).then(|| {
            TerminalFailureState {
                entry_id: entry.id.clone(),
                source: if produced_by_step {
                    TerminalFailureSource::Step
                } else {
                    TerminalFailureSource::DeferredFetch
                },
                message: message.as_ref().clone(),
            }
        })
    });
    let tool_batch = derive_tool_batch(
        &started.id,
        &operation_records,
        &own_entries,
        &entries_by_id,
        &deferred_write_ids,
    );

    Ok(LaneReductionResult {
        lane_state: LaneState {
            lane: input.slice.lane.clone(),
            leaf_id: input.leaf_id.clone(),
            operation: Some(LaneOperationState {
                id: started.id.clone(),
                kind: intent.kind(),
                intent: intent.clone(),
                aborting,
                step,
                tool_batch,
                missing_initial_messages,
                pending_steer,
                pending_follow_up,
                pending_writes,
                deferred,
                overflow_recovery_used,
                newest_own,
                targets,
            }),
            pending_next_run,
        },
        effective_configuration,
        terminal_failure,
    })
}

#[cfg(test)]
mod tests {
    use pi_core::{
        AssistantMessage, ContentBlock, DeferredHandle, ModelId, ProviderId, StopReason,
        TextContent, ToolCall, ToolCallId, ToolResultMessage, Usage, UserMessage,
    };
    use serde_json::{Map, json};

    use super::*;
    use crate::{ActiveToolsEntry, MessageEntry, ModelChangeEntry, OperationOutcome, ToolReplay};

    fn user_target(id: &str, text: &str) -> ProvisionedEntry {
        ProvisionedEntry {
            id: id.to_string(),
            entry: SessionEntry::message(Message::User(UserMessage::text(text, 1))),
        }
    }

    fn assistant_message(
        content: Vec<ContentBlock>,
        stop_reason: StopReason,
        with_deferred_handle: bool,
    ) -> Message {
        Message::assistant(AssistantMessage {
            content,
            api: "openai-responses".to_string(),
            provider: ProviderId::new("openai"),
            model: ModelId::new("test-model"),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason,
            error_message: (stop_reason == StopReason::Error).then(|| "failed".to_string()),
            deferred: with_deferred_handle.then(|| DeferredHandle {
                provider: ProviderId::new("openai"),
                model_id: ModelId::new("test-model"),
                api: "openai-responses".to_string(),
                id: "deferred-1".to_string(),
                expires_at: None,
                poll_after_ms: None,
                data: None,
            }),
            raw_stop_reason: None,
            end_turn: None,
            timestamp_ms: 1,
        })
    }

    fn assistant_target(
        id: &str,
        content: Vec<ContentBlock>,
        stop_reason: StopReason,
    ) -> ProvisionedEntry {
        ProvisionedEntry {
            id: id.to_string(),
            entry: SessionEntry::message(assistant_message(
                content,
                stop_reason,
                stop_reason == StopReason::Deferred,
            )),
        }
    }

    fn persisted(target: &ProvisionedEntry, seq: u64, parent: Option<&str>) -> SessionRecord {
        SessionRecord {
            id: target.id.clone(),
            seq,
            parent_id: parent.map(str::to_string),
            timestamp_ms: i64::try_from(seq).unwrap(),
            entry: target.entry.clone(),
        }
    }

    fn lane_record(id: &str, seq: u64, record: LaneRecordEntry) -> LaneRecord {
        LaneRecord {
            id: id.to_string(),
            seq,
            lane: "main".to_string(),
            timestamp_ms: i64::try_from(seq).unwrap(),
            record,
        }
    }

    fn run_started(seq: u64, initial_messages: Vec<ProvisionedEntry>) -> LaneRecord {
        lane_record(
            "run-1",
            seq,
            LaneRecordEntry::OperationStarted {
                source_leaf_id: None,
                intent: OperationIntent::Run {
                    original_prompt: Vec::new(),
                    initial_messages,
                    system_prompt_override: None,
                    resume_data: None,
                },
            },
        )
    }

    fn attempt(
        id: &str,
        seq: u64,
        step: StepKind,
        attempt: u32,
        result_entry_id: &str,
        compaction_reason: Option<CompactionReason>,
    ) -> LaneRecord {
        lane_record(
            id,
            seq,
            LaneRecordEntry::StepAttempt {
                run_id: "run-1".to_string(),
                step,
                attempt,
                result_entry_id: result_entry_id.to_string(),
                compaction_reason,
            },
        )
    }

    fn queue(id: &str, seq: u64, queue: QueueKind, target: ProvisionedEntry) -> LaneRecord {
        lane_record(
            id,
            seq,
            LaneRecordEntry::QueueEnqueued {
                queue,
                run_id: (queue != QueueKind::NextRun).then(|| "run-1".to_string()),
                target,
            },
        )
    }

    fn slice(records: Vec<LaneRecord>, entries: Vec<SessionRecord>) -> RecordLogSlice {
        let finished = records
            .iter()
            .filter_map(|record| match &record.record {
                LaneRecordEntry::OperationFinished { run_id, .. } => Some(run_id.as_str()),
                _ => None,
            })
            .collect::<HashSet<_>>();
        let mut open_operations = records
            .iter()
            .filter(|record| {
                matches!(record.record, LaneRecordEntry::OperationStarted { .. })
                    && !finished.contains(record.id.as_str())
            })
            .cloned()
            .collect::<Vec<_>>();
        open_operations.sort_by_key(|record| std::cmp::Reverse(record.seq));
        RecordLogSlice {
            lane: "main".to_string(),
            open_operations,
            records,
            entries,
        }
    }

    fn assert_corruption(input: RecordLogSlice, expected: RecordLogCorruptionReason) {
        assert_eq!(validate_record_log(&input).unwrap_err().reason, expected);
    }

    #[test]
    fn rejects_every_record_log_contradiction_class() {
        assert_corruption(
            slice(
                vec![
                    run_started(1, Vec::new()),
                    lane_record(
                        "run-2",
                        2,
                        LaneRecordEntry::OperationStarted {
                            source_leaf_id: None,
                            intent: OperationIntent::Run {
                                original_prompt: Vec::new(),
                                initial_messages: Vec::new(),
                                system_prompt_override: None,
                                resume_data: None,
                            },
                        },
                    ),
                ],
                Vec::new(),
            ),
            RecordLogCorruptionReason::MultipleOpenOperations,
        );
        assert_corruption(
            slice(
                vec![lane_record(
                    "abort",
                    1,
                    LaneRecordEntry::AbortRequested {
                        run_id: "missing".to_string(),
                    },
                )],
                Vec::new(),
            ),
            RecordLogCorruptionReason::UnknownOperation,
        );
        assert_corruption(
            slice(
                vec![
                    run_started(1, Vec::new()),
                    lane_record(
                        "finish",
                        2,
                        LaneRecordEntry::OperationFinished {
                            run_id: "run-1".to_string(),
                            outcome: OperationOutcome::Completed,
                            error: None,
                        },
                    ),
                    lane_record(
                        "abort",
                        3,
                        LaneRecordEntry::AbortRequested {
                            run_id: "run-1".to_string(),
                        },
                    ),
                ],
                Vec::new(),
            ),
            RecordLogCorruptionReason::RecordAfterFinish,
        );
        assert_corruption(
            slice(
                vec![
                    run_started(1, Vec::new()),
                    attempt("attempt-1", 2, StepKind::Assistant, 1, "a-1", None),
                    attempt("attempt-2", 3, StepKind::Assistant, 3, "a-2", None),
                ],
                Vec::new(),
            ),
            RecordLogCorruptionReason::NonConsecutiveAttempt,
        );
        assert_corruption(
            slice(
                vec![
                    run_started(1, Vec::new()),
                    attempt(
                        "attempt",
                        2,
                        StepKind::Assistant,
                        1,
                        "a-1",
                        Some(CompactionReason::Manual),
                    ),
                ],
                Vec::new(),
            ),
            RecordLogCorruptionReason::InvalidCompactionReason,
        );

        let queued = user_target("queued", "queued");
        assert_corruption(
            slice(
                vec![
                    run_started(1, Vec::new()),
                    lane_record(
                        "abort",
                        2,
                        LaneRecordEntry::AbortRequested {
                            run_id: "run-1".to_string(),
                        },
                    ),
                    queue("queue", 3, QueueKind::Steer, queued.clone()),
                ],
                Vec::new(),
            ),
            RecordLogCorruptionReason::QueueAfterAbort,
        );
        assert_corruption(
            slice(
                vec![
                    run_started(1, Vec::new()),
                    lane_record(
                        "cancel",
                        2,
                        LaneRecordEntry::QueueCancelled {
                            run_id: Some("run-1".to_string()),
                            entry_id: queued.id.clone(),
                        },
                    ),
                ],
                Vec::new(),
            ),
            RecordLogCorruptionReason::InvalidQueueCancellation,
        );
        assert_corruption(
            slice(
                vec![
                    run_started(1, Vec::new()),
                    attempt(
                        "compact-1",
                        2,
                        StepKind::Compaction,
                        1,
                        "summary-1",
                        Some(CompactionReason::Threshold),
                    ),
                    attempt(
                        "compact-2",
                        3,
                        StepKind::Compaction,
                        2,
                        "summary-2",
                        Some(CompactionReason::Threshold),
                    ),
                ],
                Vec::new(),
            ),
            RecordLogCorruptionReason::InconsistentStep,
        );

        let assistant_tools = assistant_target(
            "assistant-tools",
            vec![ContentBlock::ToolCall(ToolCall::new(
                "call-1",
                "tool-1",
                json!({}),
            ))],
            StopReason::ToolUse,
        );
        let assistant_entry = persisted(&assistant_tools, 3, None);
        let tool_start = |id: &str, seq: u64, call_id: &str, result_id: &str| {
            lane_record(
                id,
                seq,
                LaneRecordEntry::ToolStarted {
                    run_id: "run-1".to_string(),
                    assistant_entry_id: assistant_tools.id.clone(),
                    tool_index: 0,
                    tool_call_id: call_id.to_string(),
                    tool_name: "tool-1".to_string(),
                    effective_args: Map::new(),
                    result_entry_id: result_id.to_string(),
                    replay: ToolReplay::Never,
                },
            )
        };
        assert_corruption(
            slice(
                vec![
                    run_started(1, Vec::new()),
                    tool_start("tool", 4, "wrong", "result"),
                ],
                vec![assistant_entry.clone()],
            ),
            RecordLogCorruptionReason::ToolCallMismatch,
        );
        assert_corruption(
            slice(
                vec![
                    run_started(1, Vec::new()),
                    tool_start("tool-1", 4, "call-1", "result-1"),
                    tool_start("tool-2", 5, "call-1", "result-2"),
                ],
                vec![assistant_entry],
            ),
            RecordLogCorruptionReason::DuplicateToolInvocation,
        );

        let expected = user_target("prompt", "expected");
        let different = user_target("prompt", "different");
        assert_corruption(
            slice(
                vec![run_started(1, vec![expected])],
                vec![persisted(&different, 2, None)],
            ),
            RecordLogCorruptionReason::ProvisionedEntryMismatch,
        );
        let invalid_deferred = ProvisionedEntry {
            id: "deferred".to_string(),
            entry: SessionEntry::message(assistant_message(
                Vec::new(),
                StopReason::Deferred,
                false,
            )),
        };
        assert_corruption(
            slice(
                vec![run_started(1, Vec::new())],
                vec![persisted(&invalid_deferred, 2, None)],
            ),
            RecordLogCorruptionReason::InvalidDeferredHandle,
        );
    }

    #[test]
    fn reduces_configuration_queues_writes_and_unfinished_step() {
        let missing = user_target("prompt-missing", "missing");
        let committed = user_target("prompt-committed", "committed");
        let steer = user_target("steer", "steer");
        let consumed_follow_up = user_target("follow", "follow");
        let next_run = user_target("next", "next");
        let pending_write = user_target("write-pending", "write");
        let applied_write = user_target("write-applied", "applied");
        let start = run_started(1, vec![missing.clone(), committed.clone()]);
        let own_entries = vec![
            persisted(&committed, 2, None),
            persisted(&consumed_follow_up, 6, Some(&committed.id)),
            persisted(&applied_write, 9, Some(&consumed_follow_up.id)),
        ];
        let records = vec![
            start.clone(),
            queue("steer-record", 3, QueueKind::Steer, steer.clone()),
            queue("follow-record", 4, QueueKind::FollowUp, consumed_follow_up),
            queue("next-record", 5, QueueKind::NextRun, next_run.clone()),
            lane_record(
                "write-record",
                7,
                LaneRecordEntry::WriteDeferred {
                    run_id: "run-1".to_string(),
                    target: pending_write.clone(),
                },
            ),
            lane_record(
                "applied-write-record",
                8,
                LaneRecordEntry::WriteDeferred {
                    run_id: "run-1".to_string(),
                    target: applied_write.clone(),
                },
            ),
            attempt(
                "assistant-attempt",
                10,
                StepKind::Assistant,
                1,
                "assistant-pending",
                None,
            ),
        ];
        let configuration_entries = vec![
            SessionRecord {
                id: "model".to_string(),
                seq: 1,
                parent_id: None,
                timestamp_ms: 1,
                entry: SessionEntry::ModelChange(ModelChangeEntry {
                    provider: ProviderId::new("persisted"),
                    model_id: ModelId::new("model"),
                }),
            },
            SessionRecord {
                id: "tools".to_string(),
                seq: 2,
                parent_id: Some("model".to_string()),
                timestamp_ms: 2,
                entry: SessionEntry::ActiveToolsChange(ActiveToolsEntry {
                    active_tool_names: vec!["read".to_string()],
                }),
            },
        ];
        let result = reduce_lane_state(&LaneReductionInput {
            slice: slice(records, own_entries.clone()),
            leaf_id: Some(applied_write.id.clone()),
            own_entries,
            configuration_entries,
            defaults: EffectiveLaneConfiguration {
                model: SessionModel {
                    provider: ProviderId::new("default"),
                    model_id: ModelId::new("default"),
                },
                thinking_level: "off".to_string(),
                active_tool_names: vec!["default".to_string()],
            },
        })
        .unwrap();

        assert_eq!(result.lane_state.pending_next_run, vec![next_run]);
        let operation = result.lane_state.operation.unwrap();
        assert_eq!(operation.missing_initial_messages, vec![missing]);
        assert_eq!(operation.pending_steer, vec![steer]);
        assert!(operation.pending_follow_up.is_empty());
        assert_eq!(operation.pending_writes, vec![pending_write]);
        assert_eq!(
            operation.step,
            Some(LaneStepState {
                kind: StepKind::Assistant,
                attempts: 1,
                result_entry_id: "assistant-pending".to_string(),
                compaction_reason: None,
            })
        );
        assert_eq!(
            result.effective_configuration.active_tool_names,
            vec!["read"]
        );
        assert_eq!(
            result.effective_configuration.model.model_id.as_str(),
            "model"
        );
    }

    #[test]
    fn reduces_tool_batch_deferred_handle_and_terminal_failure() {
        let assistant_tools = assistant_target(
            "assistant-tools",
            vec![ContentBlock::ToolCall(ToolCall::new(
                "call-1",
                "tool-1",
                json!({}),
            ))],
            StopReason::ToolUse,
        );
        let tool_result = ProvisionedEntry {
            id: "tool-result".to_string(),
            entry: SessionEntry::Message(MessageEntry {
                message: Message::tool_result(ToolResultMessage {
                    tool_call_id: ToolCallId::new("call-1"),
                    tool_name: "tool-1".to_string(),
                    content: vec![ContentBlock::Text(TextContent::new("done"))],
                    details: None,
                    usage: None,
                    added_tool_names: None,
                    is_error: false,
                    timestamp_ms: 1,
                })
                .into(),
                terminate: true,
            }),
        };
        let start = run_started(1, Vec::new());
        let records = vec![
            start,
            attempt(
                "attempt",
                2,
                StepKind::Assistant,
                1,
                &assistant_tools.id,
                None,
            ),
            lane_record(
                "tool-start",
                4,
                LaneRecordEntry::ToolStarted {
                    run_id: "run-1".to_string(),
                    assistant_entry_id: assistant_tools.id.clone(),
                    tool_index: 0,
                    tool_call_id: "call-1".to_string(),
                    tool_name: "tool-1".to_string(),
                    effective_args: Map::new(),
                    result_entry_id: tool_result.id.clone(),
                    replay: ToolReplay::Never,
                },
            ),
        ];
        let own_entries = vec![
            persisted(&assistant_tools, 3, None),
            persisted(&tool_result, 5, Some(&assistant_tools.id)),
        ];
        let result = reduce_lane_state(&LaneReductionInput {
            slice: slice(records, own_entries.clone()),
            leaf_id: Some(tool_result.id.clone()),
            own_entries,
            configuration_entries: Vec::new(),
            defaults: EffectiveLaneConfiguration {
                model: SessionModel {
                    provider: ProviderId::new("default"),
                    model_id: ModelId::new("default"),
                },
                thinking_level: "off".to_string(),
                active_tool_names: Vec::new(),
            },
        })
        .unwrap();
        let batch = result.lane_state.operation.unwrap().tool_batch.unwrap();
        assert!(!batch.unresolved);
        assert_eq!(batch.calls[0].terminate, Some(true));

        let deferred = assistant_target("deferred", Vec::new(), StopReason::Deferred);
        let deferred_entry = persisted(&deferred, 3, None);
        let result = reduce_lane_state(&LaneReductionInput {
            slice: slice(
                vec![
                    run_started(1, Vec::new()),
                    attempt("attempt", 2, StepKind::Assistant, 1, &deferred.id, None),
                ],
                vec![deferred_entry.clone()],
            ),
            leaf_id: Some(deferred.id.clone()),
            own_entries: vec![deferred_entry],
            configuration_entries: Vec::new(),
            defaults: EffectiveLaneConfiguration {
                model: SessionModel {
                    provider: ProviderId::new("default"),
                    model_id: ModelId::new("default"),
                },
                thinking_level: "off".to_string(),
                active_tool_names: Vec::new(),
            },
        })
        .unwrap();
        assert_eq!(
            result.lane_state.operation.unwrap().deferred.unwrap().id,
            "deferred-1"
        );

        let failure = assistant_target("failure", Vec::new(), StopReason::Error);
        let failure_entry = persisted(&failure, 3, None);
        let result = reduce_lane_state(&LaneReductionInput {
            slice: slice(
                vec![
                    run_started(1, Vec::new()),
                    attempt("attempt", 2, StepKind::Assistant, 1, &failure.id, None),
                ],
                vec![failure_entry.clone()],
            ),
            leaf_id: Some(failure.id),
            own_entries: vec![failure_entry],
            configuration_entries: Vec::new(),
            defaults: EffectiveLaneConfiguration {
                model: SessionModel {
                    provider: ProviderId::new("default"),
                    model_id: ModelId::new("default"),
                },
                thinking_level: "off".to_string(),
                active_tool_names: Vec::new(),
            },
        })
        .unwrap();
        assert_eq!(
            result.terminal_failure.unwrap().source,
            TerminalFailureSource::Step
        );
    }
}
