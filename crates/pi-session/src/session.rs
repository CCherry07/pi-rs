use std::sync::Arc;

use pi_core::{Message, Usage, UsageCost};
use serde_json::Value;

use crate::{
    AgentMessage, BranchQuery, EntryQuery, InMemorySession, JsonlSessionMetadata, LanePointer,
    LaneRecord, LaneRecordEntry, LogItem, MAIN_LANE, NewLaneRecord, OperationIntent,
    ProvisionedEntry, RecordQuery, SessionEntry, SessionError, SessionLog, SessionMetadata,
    SessionMutation, SessionRecord, SessionStats, next_unique_id,
};

pub trait IdGenerator: Send + Sync {
    fn next(&self) -> String;
}

impl<F> IdGenerator for F
where
    F: Fn() -> String + Send + Sync,
{
    fn next(&self) -> String {
        self()
    }
}

#[derive(Default)]
pub struct DefaultIdGenerator;

impl IdGenerator for DefaultIdGenerator {
    fn next(&self) -> String {
        next_unique_id("entry")
    }
}

/// Storage seam shared by the in-memory and v4 JSONL implementations.
/// Implementations must serialize mutations and return owned snapshots.
pub trait SessionStorage: Clone + Send + Sync + 'static {
    type Metadata: Clone + Send + Sync + 'static;

    fn metadata(&self) -> Result<Self::Metadata, SessionError>;
    fn leaf_id_for_lane(&self, lane: &str) -> Result<Option<String>, SessionError>;
    fn get_entry(&self, id: &str) -> Option<SessionRecord>;
    fn lanes(&self) -> Vec<LanePointer>;
    fn create_lane(&self, lane: String, at: Option<&str>) -> Result<(), SessionError>;
    fn move_lane(&self, lane: &str, to: Option<&str>) -> Result<(), SessionError>;
    fn append_entry(
        &self,
        entry: ProvisionedEntry,
        lane: &str,
    ) -> Result<SessionRecord, SessionError>;
    fn append_record(&self, record: NewLaneRecord) -> Result<LaneRecord, SessionError>;
    fn find_entries(&self, query: &EntryQuery) -> Result<Vec<SessionRecord>, SessionError>;
    fn find_entries_on_lane_branch(
        &self,
        lane: &str,
        query: &BranchQuery,
    ) -> Result<Vec<SessionRecord>, SessionError>;
    fn find_records(&self, query: &RecordQuery) -> Result<Vec<LaneRecord>, SessionError>;
    fn find_open_operations(
        &self,
        lane: &str,
        limit: Option<usize>,
    ) -> Result<Vec<LaneRecord>, SessionError>;
    fn get_log(
        &self,
        after_seq: Option<u64>,
        limit: Option<usize>,
    ) -> Result<Vec<LogItem>, SessionError>;
    fn name(&self) -> Option<String>;
    fn set_name(&self, name: Option<String>) -> Result<(), SessionError>;
    fn label(&self, id: &str) -> Option<String>;
    fn set_label(&self, id: &str, label: Option<String>) -> Result<(), SessionError>;
    fn stats(&self) -> SessionStats;
}

#[derive(Clone)]
pub struct Session<B: SessionStorage> {
    storage: B,
    id_generator: Arc<dyn IdGenerator>,
}

impl<B: SessionStorage> Session<B> {
    pub fn new(storage: B) -> Self {
        Self::with_id_generator(storage, Arc::new(DefaultIdGenerator))
    }

    pub fn with_id_generator(storage: B, id_generator: Arc<dyn IdGenerator>) -> Self {
        Self {
            storage,
            id_generator,
        }
    }

    pub fn storage(&self) -> &B {
        &self.storage
    }

    pub fn get_metadata(&self) -> Result<B::Metadata, SessionError> {
        self.storage.metadata()
    }

    pub fn metadata(&self) -> Result<B::Metadata, SessionError> {
        self.get_metadata()
    }

    pub fn view(&self, lane: impl Into<String>) -> SessionView<B> {
        SessionView {
            session: self.clone(),
            lane: lane.into(),
        }
    }

    pub fn get_leaf_id(&self) -> Result<Option<String>, SessionError> {
        self.storage.leaf_id_for_lane(MAIN_LANE)
    }

    pub fn get_entry(&self, id: &str) -> Option<SessionRecord> {
        self.storage.get_entry(id)
    }

    pub fn get_stats(&self) -> SessionStats {
        self.storage.stats()
    }

    pub fn get_name(&self) -> Option<String> {
        self.storage.name()
    }

    pub fn set_name(&self, name: Option<String>) -> Result<(), SessionError> {
        self.storage.set_name(name)
    }

    pub fn get_label(&self, id: &str) -> Option<String> {
        self.storage.label(id)
    }

    pub fn set_label(&self, id: &str, label: Option<String>) -> Result<(), SessionError> {
        self.storage.set_label(id, label)
    }

    pub fn find_entries(&self, query: EntryQuery) -> Result<Vec<SessionRecord>, SessionError> {
        self.storage.find_entries(&query)
    }

    pub fn find_entry(&self, mut query: EntryQuery) -> Result<Option<SessionRecord>, SessionError> {
        if query.limit.is_some_and(|limit| limit == 0) {
            return self.storage.find_entries(&query).map(|_| None);
        }
        query.limit = Some(1);
        Ok(self.storage.find_entries(&query)?.into_iter().next())
    }

    pub fn find_entries_on_branch(
        &self,
        query: BranchQuery,
    ) -> Result<Vec<SessionRecord>, SessionError> {
        self.storage.find_entries_on_lane_branch(MAIN_LANE, &query)
    }

    pub fn find_entry_on_branch(
        &self,
        mut query: BranchQuery,
    ) -> Result<Option<SessionRecord>, SessionError> {
        if query.entries.limit.is_some_and(|limit| limit == 0) {
            return self
                .storage
                .find_entries_on_lane_branch(MAIN_LANE, &query)
                .map(|_| None);
        }
        query.entries.limit = Some(1);
        Ok(self
            .storage
            .find_entries_on_lane_branch(MAIN_LANE, &query)?
            .into_iter()
            .next())
    }

    pub fn append_message(&self, message: impl Into<AgentMessage>) -> Result<String, SessionError> {
        self.append_message_to_lane(MAIN_LANE, message.into())
    }

    pub fn append_custom_entry(
        &self,
        custom_type: impl Into<String>,
        data: Option<Value>,
    ) -> Result<String, SessionError> {
        self.append_custom_entry_to_lane(MAIN_LANE, custom_type.into(), data)
    }

    pub fn get_lanes(&self) -> Vec<LanePointer> {
        self.storage.lanes()
    }

    pub fn create_lane(
        &self,
        lane: impl Into<String>,
        at: Option<&str>,
    ) -> Result<(), SessionError> {
        self.storage.create_lane(lane.into(), at)
    }

    pub fn move_lane(&self, lane: &str, to: Option<&str>) -> Result<(), SessionError> {
        self.storage.move_lane(lane, to)
    }

    pub fn append_entry(
        &self,
        entry: ProvisionedEntry,
        lane: &str,
    ) -> Result<SessionRecord, SessionError> {
        validate_provisioned_entry(&entry)?;
        self.storage.append_entry(entry, lane)
    }

    pub fn append_record(&self, record: NewLaneRecord) -> Result<LaneRecord, SessionError> {
        validate_new_lane_record(&record)?;
        self.storage.append_record(record)
    }

    pub fn find_records(&self, query: RecordQuery) -> Result<Vec<LaneRecord>, SessionError> {
        validate_record_query(&query)?;
        self.storage.find_records(&query)
    }

    pub fn find_open_operations(
        &self,
        lane: &str,
        limit: Option<usize>,
    ) -> Result<Vec<LaneRecord>, SessionError> {
        self.storage.find_open_operations(lane, limit)
    }

    pub fn get_log(
        &self,
        after_seq: Option<u64>,
        limit: Option<usize>,
    ) -> Result<Vec<LogItem>, SessionError> {
        self.storage.get_log(after_seq, limit)
    }

    fn append_message_to_lane(
        &self,
        lane: &str,
        message: AgentMessage,
    ) -> Result<String, SessionError> {
        let id = self.id_generator.next();
        let entry = ProvisionedEntry {
            id: id.clone(),
            entry: crate::SessionEntry::message(message),
        };
        validate_provisioned_entry(&entry)?;
        self.storage.append_entry(entry, lane)?;
        Ok(id)
    }

    fn append_custom_entry_to_lane(
        &self,
        lane: &str,
        custom_type: String,
        data: Option<Value>,
    ) -> Result<String, SessionError> {
        let id = self.id_generator.next();
        let entry = ProvisionedEntry {
            id: id.clone(),
            entry: crate::SessionEntry::Custom(crate::CustomEntry { custom_type, data }),
        };
        validate_provisioned_entry(&entry)?;
        self.storage.append_entry(entry, lane)?;
        Ok(id)
    }
}

fn validate_json(value: &impl serde::Serialize) -> Result<(), SessionError> {
    serde_json::to_value(value)
        .map(|_| ())
        .map_err(|error| SessionError::InvalidPayload(error.to_string()))
}

pub(crate) fn validate_provisioned_entry(value: &ProvisionedEntry) -> Result<(), SessionError> {
    validate_entry_payload(&value.entry)?;
    validate_json(value)
}

pub(crate) fn validate_new_lane_record(value: &NewLaneRecord) -> Result<(), SessionError> {
    validate_record_payload(&value.record)?;
    validate_json(value)
}

pub(crate) fn validate_mutation_payload(value: &SessionMutation) -> Result<(), SessionError> {
    match value {
        SessionMutation::Entry { record, .. } => validate_entry_payload(&record.entry),
        SessionMutation::Record { record } => validate_record_payload(&record.record),
        SessionMutation::Lane { .. } | SessionMutation::Fact { .. } => Ok(()),
    }
}

fn validate_entry_payload(entry: &SessionEntry) -> Result<(), SessionError> {
    match entry {
        SessionEntry::Message(message) => validate_agent_message(&message.message),
        SessionEntry::Compaction(compaction) => {
            for message in &compaction.retained_tail {
                validate_agent_message(message)?;
            }
            if let Some(usage) = &compaction.usage {
                validate_usage(usage)?;
            }
            Ok(())
        }
        SessionEntry::BranchSummary(summary) => {
            if let Some(usage) = &summary.usage {
                validate_usage(usage)?;
            }
            Ok(())
        }
        SessionEntry::ModelChange(_)
        | SessionEntry::ThinkingLevelChange(_)
        | SessionEntry::ActiveToolsChange(_)
        | SessionEntry::Custom(_) => Ok(()),
    }
}

fn validate_record_payload(record: &LaneRecordEntry) -> Result<(), SessionError> {
    match record {
        LaneRecordEntry::OperationStarted { intent, .. } => {
            if let OperationIntent::Run {
                original_prompt,
                initial_messages,
                ..
            } = intent
            {
                for message in original_prompt {
                    validate_agent_message(message)?;
                }
                for entry in initial_messages {
                    validate_entry_payload(&entry.entry)?;
                }
            }
            Ok(())
        }
        LaneRecordEntry::StepAttempt {
            step,
            compaction_reason,
            ..
        } => {
            let valid = matches!(
                (step, compaction_reason),
                (crate::StepKind::Compaction, Some(_))
                    | (
                        crate::StepKind::Assistant | crate::StepKind::BranchSummary,
                        None
                    )
            );
            if valid {
                Ok(())
            } else {
                Err(SessionError::InvalidPayload(
                    "compactionReason must exist exactly for compaction step attempts".to_string(),
                ))
            }
        }
        LaneRecordEntry::QueueEnqueued {
            queue,
            run_id,
            target,
        } => {
            let valid = matches!(
                (queue, run_id),
                (crate::QueueKind::NextRun, None)
                    | (
                        crate::QueueKind::Steer | crate::QueueKind::FollowUp,
                        Some(_)
                    )
            );
            if !valid {
                return Err(SessionError::InvalidPayload(
                    "runId must exist for steer/followUp queues and be absent for nextRun"
                        .to_string(),
                ));
            }
            validate_entry_payload(&target.entry)
        }
        LaneRecordEntry::WriteDeferred { target, .. } => validate_entry_payload(&target.entry),
        LaneRecordEntry::Usage(usage) => validate_usage_cost(&usage.usage.cost),
        LaneRecordEntry::AbortRequested { .. }
        | LaneRecordEntry::OperationFinished { .. }
        | LaneRecordEntry::ToolStarted { .. }
        | LaneRecordEntry::QueueCancelled { .. } => Ok(()),
    }
}

fn validate_agent_message(message: &AgentMessage) -> Result<(), SessionError> {
    match message.as_standard() {
        Some(Message::Assistant(message)) => validate_usage(&message.usage),
        Some(Message::ToolResult(message)) => {
            if let Some(usage) = &message.usage {
                validate_usage(usage)?;
            }
            Ok(())
        }
        Some(Message::User(_)) | None => Ok(()),
    }
}

fn validate_usage(usage: &Usage) -> Result<(), SessionError> {
    validate_usage_cost(&usage.cost)
}

fn validate_usage_cost(cost: &UsageCost) -> Result<(), SessionError> {
    let values = [
        cost.input,
        cost.output,
        cost.cache_read,
        cost.cache_write,
        cost.total,
    ];
    if values.into_iter().all(f64::is_finite) {
        Ok(())
    } else {
        Err(SessionError::InvalidPayload(
            "durable payload contains a non-finite number".to_string(),
        ))
    }
}

fn validate_record_query(query: &RecordQuery) -> Result<(), SessionError> {
    if query.operation_kind.is_some()
        && query.record_type != Some(crate::LaneRecordType::OperationStarted)
    {
        return Err(SessionError::InvalidQuery(
            "operation_kind requires operation_started record type".to_string(),
        ));
    }
    Ok(())
}

#[derive(Clone)]
pub struct SessionView<B: SessionStorage> {
    session: Session<B>,
    lane: String,
}

impl<B: SessionStorage> SessionView<B> {
    pub fn lane(&self) -> &str {
        &self.lane
    }

    pub fn get_leaf_id(&self) -> Result<Option<String>, SessionError> {
        self.session.storage.leaf_id_for_lane(&self.lane)
    }

    pub fn get_entry(&self, id: &str) -> Option<SessionRecord> {
        self.session.get_entry(id)
    }

    pub fn get_stats(&self) -> SessionStats {
        self.session.get_stats()
    }

    pub fn get_name(&self) -> Option<String> {
        self.session.get_name()
    }

    pub fn set_name(&self, name: Option<String>) -> Result<(), SessionError> {
        self.session.set_name(name)
    }

    pub fn get_label(&self, id: &str) -> Option<String> {
        self.session.get_label(id)
    }

    pub fn set_label(&self, id: &str, label: Option<String>) -> Result<(), SessionError> {
        self.session.set_label(id, label)
    }

    pub fn find_entries(&self, query: EntryQuery) -> Result<Vec<SessionRecord>, SessionError> {
        self.session.find_entries(query)
    }

    pub fn find_entry(&self, query: EntryQuery) -> Result<Option<SessionRecord>, SessionError> {
        self.session.find_entry(query)
    }

    pub fn find_entries_on_branch(
        &self,
        query: BranchQuery,
    ) -> Result<Vec<SessionRecord>, SessionError> {
        self.session
            .storage
            .find_entries_on_lane_branch(&self.lane, &query)
    }

    pub fn find_entry_on_branch(
        &self,
        mut query: BranchQuery,
    ) -> Result<Option<SessionRecord>, SessionError> {
        if query.entries.limit.is_some_and(|limit| limit == 0) {
            return self
                .session
                .storage
                .find_entries_on_lane_branch(&self.lane, &query)
                .map(|_| None);
        }
        query.entries.limit = Some(1);
        Ok(self
            .session
            .storage
            .find_entries_on_lane_branch(&self.lane, &query)?
            .into_iter()
            .next())
    }

    pub fn append_message(&self, message: impl Into<AgentMessage>) -> Result<String, SessionError> {
        self.session
            .append_message_to_lane(&self.lane, message.into())
    }

    pub fn append_custom_entry(
        &self,
        custom_type: impl Into<String>,
        data: Option<Value>,
    ) -> Result<String, SessionError> {
        self.session
            .append_custom_entry_to_lane(&self.lane, custom_type.into(), data)
    }
}

impl SessionStorage for SessionLog {
    type Metadata = JsonlSessionMetadata;

    fn metadata(&self) -> Result<Self::Metadata, SessionError> {
        SessionLog::metadata(self)
    }

    fn leaf_id_for_lane(&self, lane: &str) -> Result<Option<String>, SessionError> {
        SessionLog::leaf_id_for_lane(self, lane)
    }

    fn get_entry(&self, id: &str) -> Option<SessionRecord> {
        SessionLog::get_entry(self, id)
    }

    fn lanes(&self) -> Vec<LanePointer> {
        SessionLog::lanes(self)
    }

    fn create_lane(&self, lane: String, at: Option<&str>) -> Result<(), SessionError> {
        SessionLog::create_lane(self, lane, at)
    }

    fn move_lane(&self, lane: &str, to: Option<&str>) -> Result<(), SessionError> {
        SessionLog::move_lane(self, lane, to)
    }

    fn append_entry(
        &self,
        entry: ProvisionedEntry,
        lane: &str,
    ) -> Result<SessionRecord, SessionError> {
        SessionLog::append_entry(self, entry, lane)
    }

    fn append_record(&self, record: NewLaneRecord) -> Result<LaneRecord, SessionError> {
        SessionLog::append_record(self, record)
    }

    fn find_entries(&self, query: &EntryQuery) -> Result<Vec<SessionRecord>, SessionError> {
        SessionLog::find_entries(self, query)
    }

    fn find_entries_on_lane_branch(
        &self,
        lane: &str,
        query: &BranchQuery,
    ) -> Result<Vec<SessionRecord>, SessionError> {
        SessionLog::find_entries_on_lane_branch(self, lane, query)
    }

    fn find_records(&self, query: &RecordQuery) -> Result<Vec<LaneRecord>, SessionError> {
        SessionLog::find_records(self, query)
    }

    fn find_open_operations(
        &self,
        lane: &str,
        limit: Option<usize>,
    ) -> Result<Vec<LaneRecord>, SessionError> {
        SessionLog::find_open_operations(self, lane, limit)
    }

    fn get_log(
        &self,
        after_seq: Option<u64>,
        limit: Option<usize>,
    ) -> Result<Vec<LogItem>, SessionError> {
        SessionLog::get_log(self, after_seq, limit)
    }

    fn name(&self) -> Option<String> {
        SessionLog::name(self)
    }

    fn set_name(&self, name: Option<String>) -> Result<(), SessionError> {
        SessionLog::set_name(self, name)
    }

    fn label(&self, id: &str) -> Option<String> {
        SessionLog::label(self, id)
    }

    fn set_label(&self, id: &str, label: Option<String>) -> Result<(), SessionError> {
        SessionLog::set_label(self, id, label)
    }

    fn stats(&self) -> SessionStats {
        SessionLog::stats(self)
    }
}

impl SessionStorage for InMemorySession {
    type Metadata = SessionMetadata;

    fn metadata(&self) -> Result<Self::Metadata, SessionError> {
        Ok(InMemorySession::metadata(self))
    }

    fn leaf_id_for_lane(&self, lane: &str) -> Result<Option<String>, SessionError> {
        InMemorySession::leaf_id_for_lane(self, lane)
    }

    fn get_entry(&self, id: &str) -> Option<SessionRecord> {
        InMemorySession::get_entry(self, id)
    }

    fn lanes(&self) -> Vec<LanePointer> {
        InMemorySession::lanes(self)
    }

    fn create_lane(&self, lane: String, at: Option<&str>) -> Result<(), SessionError> {
        InMemorySession::create_lane(self, lane, at)
    }

    fn move_lane(&self, lane: &str, to: Option<&str>) -> Result<(), SessionError> {
        InMemorySession::move_lane(self, lane, to)
    }

    fn append_entry(
        &self,
        entry: ProvisionedEntry,
        lane: &str,
    ) -> Result<SessionRecord, SessionError> {
        InMemorySession::append_entry(self, entry, lane)
    }

    fn append_record(&self, record: NewLaneRecord) -> Result<LaneRecord, SessionError> {
        InMemorySession::append_record(self, record)
    }

    fn find_entries(&self, query: &EntryQuery) -> Result<Vec<SessionRecord>, SessionError> {
        InMemorySession::find_entries(self, query)
    }

    fn find_entries_on_lane_branch(
        &self,
        lane: &str,
        query: &BranchQuery,
    ) -> Result<Vec<SessionRecord>, SessionError> {
        InMemorySession::find_entries_on_lane_branch(self, lane, query)
    }

    fn find_records(&self, query: &RecordQuery) -> Result<Vec<LaneRecord>, SessionError> {
        InMemorySession::find_records(self, query)
    }

    fn find_open_operations(
        &self,
        lane: &str,
        limit: Option<usize>,
    ) -> Result<Vec<LaneRecord>, SessionError> {
        InMemorySession::find_open_operations(self, lane, limit)
    }

    fn get_log(
        &self,
        after_seq: Option<u64>,
        limit: Option<usize>,
    ) -> Result<Vec<LogItem>, SessionError> {
        InMemorySession::get_log(self, after_seq, limit)
    }

    fn name(&self) -> Option<String> {
        InMemorySession::name(self)
    }

    fn set_name(&self, name: Option<String>) -> Result<(), SessionError> {
        InMemorySession::set_name(self, name)
    }

    fn label(&self, id: &str) -> Option<String> {
        InMemorySession::label(self, id)
    }

    fn set_label(&self, id: &str, label: Option<String>) -> Result<(), SessionError> {
        InMemorySession::set_label(self, id, label)
    }

    fn stats(&self) -> SessionStats {
        InMemorySession::stats(self)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use pi_core::{
        AssistantMessage, Message, ModelId, ProviderId, StopReason, Usage, UsageCost, UserMessage,
    };

    use super::*;
    use crate::{
        EntryOrder, LaneRecordEntry, OperationIntent, OperationOutcome, SessionEntry,
        SessionEntryType, SessionUsage, UsageAttribution, UsageRecord,
    };

    fn memory_session() -> Session<InMemorySession> {
        Session::new(InMemorySession::new(SessionMetadata {
            id: "session".to_string(),
            created_at: 1,
            parent_session_id: None,
        }))
    }

    fn started(id: &str, lane: &str) -> NewLaneRecord {
        NewLaneRecord {
            id: id.to_string(),
            lane: lane.to_string(),
            record: LaneRecordEntry::OperationStarted {
                source_leaf_id: None,
                intent: OperationIntent::Run {
                    original_prompt: Vec::new(),
                    initial_messages: Vec::new(),
                    system_prompt_override: None,
                    resume_data: None,
                },
            },
        }
    }

    #[test]
    fn one_injected_id_generator_is_shared_by_lane_views() {
        let next = Arc::new(AtomicUsize::new(0));
        let generator = {
            let next = Arc::clone(&next);
            Arc::new(move || format!("generated-{}", next.fetch_add(1, Ordering::SeqCst) + 1))
                as Arc<dyn IdGenerator>
        };
        let storage = InMemorySession::new(SessionMetadata {
            id: "session".to_string(),
            created_at: 1,
            parent_session_id: None,
        });
        let session = Session::with_id_generator(storage, generator);
        let root = session
            .append_message(Message::User(UserMessage::text("root", 1)))
            .unwrap();
        session.create_lane("thread", Some(&root)).unwrap();
        let child = session
            .view("thread")
            .append_custom_entry("note", None)
            .unwrap();
        assert_eq!(root, "generated-1");
        assert_eq!(child, "generated-2");
    }

    #[test]
    fn all_mutation_kinds_share_one_sequence_and_lanes_share_one_tree() {
        let session = memory_session();
        let root = session
            .append_entry(
                ProvisionedEntry {
                    id: "root".to_string(),
                    entry: SessionEntry::message(Message::User(UserMessage::text("root", 1))),
                },
                MAIN_LANE,
            )
            .unwrap();
        session.create_lane("thread", Some(&root.id)).unwrap();
        let child = session
            .append_entry(
                ProvisionedEntry {
                    id: "child".to_string(),
                    entry: SessionEntry::Custom(crate::CustomEntry {
                        custom_type: "note".to_string(),
                        data: Some(serde_json::json!({"value": 1})),
                    }),
                },
                "thread",
            )
            .unwrap();
        let operation = session.append_record(started("run", "thread")).unwrap();
        session.set_name(Some("Example".to_string())).unwrap();
        session
            .set_label(&root.id, Some("checkpoint".to_string()))
            .unwrap();
        session.move_lane(MAIN_LANE, Some(&child.id)).unwrap();

        assert_eq!((root.parent_id, root.seq), (None, 1));
        assert_eq!((child.parent_id.as_deref(), child.seq), (Some("root"), 3));
        assert_eq!(operation.seq, 4);
        assert_eq!(
            session
                .get_log(None, None)
                .unwrap()
                .iter()
                .map(LogItem::seq)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5, 6, 7]
        );
        assert_eq!(session.get_lanes()[0].leaf_id.as_deref(), Some("child"));
        assert_eq!(session.get_lanes()[1].leaf_id.as_deref(), Some("child"));
    }

    #[test]
    fn record_queries_and_open_operation_invariant_match_pi() {
        let session = memory_session();
        assert!(
            session
                .find_open_operations("missing", Some(2))
                .unwrap()
                .is_empty()
        );
        let invalid = session
            .find_records(RecordQuery {
                operation_kind: Some(crate::OperationKind::Run),
                ..RecordQuery::default()
            })
            .unwrap_err();
        assert_eq!(invalid.code(), crate::SessionErrorCode::InvalidQuery);
        let first = session.append_record(started("first", MAIN_LANE)).unwrap();
        assert_eq!(
            session.find_open_operations(MAIN_LANE, Some(2)).unwrap(),
            vec![first.clone()]
        );
        assert!(matches!(
            session.append_record(started("second", MAIN_LANE)),
            Err(SessionError::Storage(_))
        ));
        session
            .append_record(NewLaneRecord {
                id: "finish".to_string(),
                lane: MAIN_LANE.to_string(),
                record: LaneRecordEntry::OperationFinished {
                    run_id: first.id,
                    outcome: OperationOutcome::Completed,
                    error: None,
                },
            })
            .unwrap();
        assert!(
            session
                .find_open_operations(MAIN_LANE, Some(2))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn rejects_non_finite_usage_before_mutating_storage() {
        let session = memory_session();
        let error = session
            .append_record(NewLaneRecord {
                id: "invalid-usage".to_string(),
                lane: MAIN_LANE.to_string(),
                record: LaneRecordEntry::Usage(UsageRecord {
                    usage: SessionUsage {
                        cost: UsageCost {
                            total: f64::NAN,
                            ..UsageCost::default()
                        },
                        ..SessionUsage::default()
                    },
                    attribution: UsageAttribution::Adjustment {
                        run_id: None,
                        entry_id: None,
                        details: None,
                    },
                }),
            })
            .unwrap_err();
        assert_eq!(error.code(), crate::SessionErrorCode::InvalidPayload);
        assert!(session.get_log(None, None).unwrap().is_empty());
    }

    #[test]
    fn filtered_branch_queries_and_signed_usage_ledger_match_pi() {
        let session = memory_session();
        session
            .append_entry(
                ProvisionedEntry {
                    id: "root".to_string(),
                    entry: SessionEntry::message(Message::User(UserMessage::text("root", 1))),
                },
                MAIN_LANE,
            )
            .unwrap();
        session
            .append_entry(
                ProvisionedEntry {
                    id: "note".to_string(),
                    entry: SessionEntry::Custom(crate::CustomEntry {
                        custom_type: "note".to_string(),
                        data: None,
                    }),
                },
                MAIN_LANE,
            )
            .unwrap();
        let assistant = Message::assistant(AssistantMessage {
            content: Vec::new(),
            api: "test".to_string(),
            provider: ProviderId::new("provider"),
            model: ModelId::new("model"),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: Usage {
                input: 10,
                output: 5,
                cache_read: 3,
                cache_write: 2,
                cache_write_1h: None,
                reasoning: None,
                total_tokens: 20,
                cost: UsageCost {
                    input: 1.0,
                    output: 2.0,
                    cache_read: 3.0,
                    cache_write: 4.0,
                    total: 10.0,
                },
            },
            stop_reason: StopReason::Stop,
            error_message: None,
            deferred: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp_ms: 1,
        });
        session
            .append_entry(
                ProvisionedEntry {
                    id: "assistant".to_string(),
                    entry: SessionEntry::message(assistant),
                },
                MAIN_LANE,
            )
            .unwrap();
        session
            .append_record(NewLaneRecord {
                id: "usage".to_string(),
                lane: MAIN_LANE.to_string(),
                record: LaneRecordEntry::Usage(UsageRecord {
                    usage: SessionUsage {
                        input: 10,
                        output: 5,
                        cache_read: 3,
                        cache_write: 2,
                        cache_write_1h: None,
                        reasoning: None,
                        total_tokens: 20,
                        cost: UsageCost {
                            total: 10.0,
                            ..UsageCost::default()
                        },
                    },
                    attribution: UsageAttribution::Assistant {
                        run_id: "run".to_string(),
                        entry_id: "assistant".to_string(),
                        attempt: 1,
                        stop_reason: crate::SessionStopReason::Stop,
                    },
                }),
            })
            .unwrap();
        session
            .append_record(NewLaneRecord {
                id: "adjustment".to_string(),
                lane: MAIN_LANE.to_string(),
                record: LaneRecordEntry::Usage(UsageRecord {
                    usage: SessionUsage {
                        input: -2,
                        total_tokens: -2,
                        cost: UsageCost {
                            total: -0.5,
                            ..UsageCost::default()
                        },
                        ..SessionUsage::default()
                    },
                    attribution: UsageAttribution::Adjustment {
                        run_id: None,
                        entry_id: None,
                        details: None,
                    },
                }),
            })
            .unwrap();

        assert_eq!(
            session
                .find_entries(EntryQuery {
                    custom_type: Some("note".to_string()),
                    ..EntryQuery::default()
                })
                .unwrap()
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            vec!["note"]
        );
        assert_eq!(
            session
                .find_entries_on_branch(BranchQuery {
                    stop_at_type: Some(SessionEntryType::Custom),
                    entries: EntryQuery {
                        order: EntryOrder::OldestFirst,
                        ..EntryQuery::default()
                    },
                    ..BranchQuery::default()
                })
                .unwrap()
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            vec!["root", "note"]
        );
        assert_eq!(
            session.get_stats(),
            SessionStats {
                message_count: 2,
                cached_tokens: 3,
                uncached_tokens: 10,
                total_tokens: 18,
                cost_total: 9.5,
            }
        );
    }
}
