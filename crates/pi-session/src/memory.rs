use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::Value;

use crate::session::{validate_new_lane_record, validate_provisioned_entry};
use crate::state::SessionState;
use crate::{
    AgentMessage, BranchQuery, CustomEntry, EntryQuery, ForkOptions, LanePointer, LaneRecord,
    LaneRecordEntry, LogItem, MAIN_LANE, NewLaneRecord, ProvisionedEntry, RecordQuery, Session,
    SessionCreateOptions, SessionEntry, SessionError, SessionFact, SessionMetadata,
    SessionMutation, SessionRecord, SessionStats, next_unique_id, now_ms,
};

#[derive(Clone)]
pub struct InMemorySession {
    metadata: SessionMetadata,
    state: Arc<Mutex<SessionState>>,
}

impl InMemorySession {
    pub fn new(metadata: SessionMetadata) -> Self {
        Self {
            metadata,
            state: Arc::new(Mutex::new(SessionState::default())),
        }
    }

    fn from_state(metadata: SessionMetadata, state: SessionState) -> Self {
        Self {
            metadata,
            state: Arc::new(Mutex::new(state)),
        }
    }

    pub fn metadata(&self) -> SessionMetadata {
        self.metadata.clone()
    }

    pub fn leaf_id(&self) -> Option<String> {
        self.leaf_id_for_lane(MAIN_LANE).ok().flatten()
    }

    pub fn leaf_id_for_lane(&self, lane: &str) -> Result<Option<String>, SessionError> {
        self.state().require_lane(lane)
    }

    pub fn get_entry(&self, id: &str) -> Option<SessionRecord> {
        self.state().get_entry(id)
    }

    pub fn lanes(&self) -> Vec<LanePointer> {
        self.state().lanes()
    }

    pub fn create_lane(
        &self,
        lane: impl Into<String>,
        at: Option<&str>,
    ) -> Result<(), SessionError> {
        let lane = lane.into();
        let mut state = self.state();
        state.validate_new_lane(&lane)?;
        state.validate_target(at)?;
        let seq = state.next_sequence();
        state.apply_mutation(SessionMutation::Lane {
            seq,
            lane,
            leaf_id: at.map(str::to_string),
        })
    }

    pub fn move_lane(&self, lane: &str, to: Option<&str>) -> Result<(), SessionError> {
        let mut state = self.state();
        state.require_lane(lane)?;
        state.validate_target(to)?;
        let seq = state.next_sequence();
        state.apply_mutation(SessionMutation::Lane {
            seq,
            lane: lane.to_string(),
            leaf_id: to.map(str::to_string),
        })
    }

    pub fn append_entry(
        &self,
        provisioned: ProvisionedEntry,
        lane: &str,
    ) -> Result<SessionRecord, SessionError> {
        validate_provisioned_entry(&provisioned)?;
        let mut state = self.state();
        let parent_id = state.require_lane(lane)?;
        state.validate_unused_id(&provisioned.id)?;
        let record = SessionRecord {
            id: provisioned.id,
            seq: state.next_sequence(),
            parent_id,
            timestamp_ms: now_ms(),
            entry: provisioned.entry,
        };
        state.apply_mutation(SessionMutation::Entry {
            lane: Some(lane.to_string()),
            record: record.clone(),
        })?;
        Ok(record)
    }

    pub fn append_to_lane(&self, entry: SessionEntry, lane: &str) -> Result<String, SessionError> {
        let id = next_unique_id("entry");
        self.append_entry(
            ProvisionedEntry {
                id: id.clone(),
                entry,
            },
            lane,
        )?;
        Ok(id)
    }

    pub fn append(&self, entry: SessionEntry) -> Result<String, SessionError> {
        self.append_to_lane(entry, MAIN_LANE)
    }

    pub fn append_message(&self, message: impl Into<AgentMessage>) -> Result<String, SessionError> {
        self.append(SessionEntry::message(message))
    }

    pub fn append_custom_entry(
        &self,
        custom_type: impl Into<String>,
        data: Option<Value>,
    ) -> Result<String, SessionError> {
        self.append(SessionEntry::Custom(CustomEntry {
            custom_type: custom_type.into(),
            data,
        }))
    }

    pub fn append_record(&self, new_record: NewLaneRecord) -> Result<LaneRecord, SessionError> {
        validate_new_lane_record(&new_record)?;
        let mut state = self.state();
        state.require_lane(&new_record.lane)?;
        state.validate_unused_id(&new_record.id)?;
        if matches!(new_record.record, LaneRecordEntry::OperationStarted { .. })
            && let Some(open) = state
                .find_open_operations(&new_record.lane, Some(1))?
                .first()
        {
            return Err(SessionError::Storage(format!(
                "lane {} already has an open operation {}",
                new_record.lane, open.id
            )));
        }
        let record = LaneRecord {
            id: new_record.id,
            seq: state.next_sequence(),
            lane: new_record.lane,
            timestamp_ms: now_ms(),
            record: new_record.record,
        };
        state.apply_mutation(SessionMutation::Record {
            record: record.clone(),
        })?;
        Ok(record)
    }

    pub fn find_entries(&self, query: &EntryQuery) -> Result<Vec<SessionRecord>, SessionError> {
        self.state().find_entries(query)
    }

    pub fn find_entries_on_branch(
        &self,
        query: &BranchQuery,
    ) -> Result<Vec<SessionRecord>, SessionError> {
        self.find_entries_on_lane_branch(MAIN_LANE, query)
    }

    pub fn find_entries_on_lane_branch(
        &self,
        lane: &str,
        query: &BranchQuery,
    ) -> Result<Vec<SessionRecord>, SessionError> {
        self.state().find_entries_on_branch(query, lane)
    }

    pub fn find_records(&self, query: &RecordQuery) -> Result<Vec<LaneRecord>, SessionError> {
        self.state().find_records(query)
    }

    pub fn find_open_operations(
        &self,
        lane: &str,
        limit: Option<usize>,
    ) -> Result<Vec<LaneRecord>, SessionError> {
        self.state().find_open_operations(lane, limit)
    }

    pub fn get_log(
        &self,
        after_seq: Option<u64>,
        limit: Option<usize>,
    ) -> Result<Vec<LogItem>, SessionError> {
        self.state().get_log(after_seq, limit)
    }

    pub fn name(&self) -> Option<String> {
        self.state().name()
    }

    pub fn set_name(&self, name: Option<String>) -> Result<(), SessionError> {
        let mut state = self.state();
        let seq = state.next_sequence();
        state.apply_mutation(SessionMutation::Fact {
            seq,
            fact: SessionFact::Name { name },
        })
    }

    pub fn label(&self, id: &str) -> Option<String> {
        self.state().label(id)
    }

    pub fn set_label(&self, id: &str, label: Option<String>) -> Result<(), SessionError> {
        let mut state = self.state();
        state.validate_target(Some(id))?;
        let seq = state.next_sequence();
        state.apply_mutation(SessionMutation::Fact {
            seq,
            fact: SessionFact::Label {
                target_id: id.to_string(),
                label,
            },
        })
    }

    pub fn stats(&self) -> SessionStats {
        self.state().stats()
    }

    fn fork(&self, metadata: SessionMetadata, options: &ForkOptions) -> Result<Self, SessionError> {
        let mutations = self.state().create_fork_mutations(options)?;
        let mut state = SessionState::default();
        for mutation in mutations {
            state.apply_mutation(mutation)?;
        }
        Ok(Self::from_state(metadata, state))
    }

    fn state(&self) -> MutexGuard<'_, SessionState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[derive(Clone, Default)]
pub struct InMemorySessionRepo {
    sessions: Arc<Mutex<Vec<(String, InMemorySession)>>>,
}

impl InMemorySessionRepo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create(
        &self,
        options: SessionCreateOptions,
    ) -> Result<Session<InMemorySession>, SessionError> {
        let mut sessions = self.sessions();
        let id = options.id.unwrap_or_else(|| next_unique_id("session"));
        if sessions.iter().any(|(candidate, _)| candidate == &id) {
            return Err(SessionError::AlreadyExists(id));
        }
        let session = InMemorySession::new(SessionMetadata {
            id: id.clone(),
            created_at: now_ms(),
            parent_session_id: options.parent_session_id,
        });
        sessions.push((id, session.clone()));
        Ok(Session::new(session))
    }

    pub fn open(
        &self,
        metadata: &SessionMetadata,
    ) -> Result<Session<InMemorySession>, SessionError> {
        self.sessions()
            .iter()
            .find(|(id, _)| id == &metadata.id)
            .map(|(_, session)| Session::new(session.clone()))
            .ok_or_else(|| SessionError::NotFound(metadata.id.clone()))
    }

    pub fn list(&self) -> Vec<SessionMetadata> {
        self.sessions()
            .iter()
            .map(|(_, session)| session.metadata())
            .collect()
    }

    pub fn delete(&self, metadata: &SessionMetadata) {
        self.sessions().retain(|(id, _)| id != &metadata.id);
    }

    pub fn fork(
        &self,
        source: &SessionMetadata,
        options: &ForkOptions,
        create: SessionCreateOptions,
    ) -> Result<Session<InMemorySession>, SessionError> {
        let mut sessions = self.sessions();
        let source_session = sessions
            .iter()
            .find(|(id, _)| id == &source.id)
            .map(|(_, session)| session.clone())
            .ok_or_else(|| SessionError::NotFound(source.id.clone()))?;
        let id = create.id.unwrap_or_else(|| next_unique_id("session"));
        if sessions.iter().any(|(candidate, _)| candidate == &id) {
            return Err(SessionError::AlreadyExists(id));
        }
        let session = source_session.fork(
            SessionMetadata {
                id: id.clone(),
                created_at: now_ms(),
                parent_session_id: create.parent_session_id.or_else(|| Some(source.id.clone())),
            },
            options,
        )?;
        sessions.push((id, session.clone()));
        Ok(Session::new(session))
    }

    fn sessions(&self) -> MutexGuard<'_, Vec<(String, InMemorySession)>> {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use pi_core::{Message, UserMessage};

    use super::*;
    use crate::{ForkPosition, SessionEntryType};

    #[test]
    fn memory_backend_shares_sequence_and_forks_only_the_selected_branch() {
        let repo = InMemorySessionRepo::new();
        let session = repo.create(SessionCreateOptions::default()).unwrap();
        let first = session
            .append_message(Message::User(UserMessage::text("first", 1)))
            .unwrap();
        session
            .append_message(Message::User(UserMessage::text("second", 2)))
            .unwrap();
        session.set_name(Some("named".to_string())).unwrap();

        let fork = repo
            .fork(
                &session.metadata().unwrap(),
                &ForkOptions::Branch {
                    entry_id: Some(first),
                    position: Some(ForkPosition::At),
                },
                SessionCreateOptions::default(),
            )
            .unwrap();
        assert_eq!(
            fork.find_entries(EntryQuery {
                entry_type: Some(SessionEntryType::Message),
                ..EntryQuery::default()
            })
            .unwrap()
            .len(),
            1
        );
        assert_eq!(fork.get_name().as_deref(), Some("named"));
    }
}
