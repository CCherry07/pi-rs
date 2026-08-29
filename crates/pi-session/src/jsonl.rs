use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::Serialize;
use serde_json::Value;

use crate::session::{
    validate_mutation_payload, validate_new_lane_record, validate_provisioned_entry,
};
use crate::state::SessionState;
use crate::{
    AgentMessage, BranchQuery, EntryQuery, ForkOptions, JsonlSessionMetadata, LanePointer,
    LaneRecord, LaneRecordEntry, LogItem, MAIN_LANE, NewLaneRecord, ProvisionedEntry, RecordQuery,
    SESSION_SCHEMA_VERSION, SessionDocument, SessionEntry, SessionError, SessionFact,
    SessionHeader, SessionMutation, SessionRecord, SessionStats, next_unique_id, now_ms,
};

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Clone)]
pub struct SessionLog {
    inner: Arc<SessionLogInner>,
}

impl std::fmt::Debug for SessionLog {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionLog")
            .field("path", &self.inner.path)
            .field("session_id", &self.inner.header.id)
            .field("materialized", &self.is_materialized())
            .finish()
    }
}

struct SessionLogInner {
    path: PathBuf,
    header: SessionHeader,
    metadata: JsonlSessionMetadata,
    state: Mutex<SessionState>,
    persistence: Mutex<SessionPersistence>,
}

enum SessionPersistence {
    Materialized,
    Deferred { encoded_mutations: Vec<u8> },
}

impl SessionLog {
    pub fn create(path: impl Into<PathBuf>, header: SessionHeader) -> Result<Self, SessionError> {
        validate_header(&header)?;
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)?;
        write_json_line(&mut file, &header, 1)?;
        file.sync_all()?;
        let modified_at = file_modified_at(&path)?;
        Ok(Self::from_parts(
            path,
            header,
            SessionState::default(),
            modified_at,
            SessionPersistence::Materialized,
        ))
    }

    /// Creates an in-memory session log whose JSONL file is written only when
    /// [`SessionLog::materialize`] is called.
    pub fn create_deferred(
        path: impl Into<PathBuf>,
        header: SessionHeader,
    ) -> Result<Self, SessionError> {
        validate_header(&header)?;
        let path = path.into();
        if path.exists() {
            return Err(SessionError::AlreadyExists(path.display().to_string()));
        }
        let modified_at = header.created_at as f64;
        Ok(Self::from_parts(
            path,
            header,
            SessionState::default(),
            modified_at,
            SessionPersistence::Deferred {
                encoded_mutations: Vec::new(),
            },
        ))
    }

    pub fn open(path: impl Into<PathBuf>) -> Result<(Self, SessionDocument), SessionError> {
        let path = path.into();
        let (header, state, repair) = load_file(&path)?;
        let modified_at = file_modified_at(&path)?;
        match repair {
            TailRepair::None => {}
            TailRepair::AppendNewline => {
                let mut file = OpenOptions::new().append(true).open(&path)?;
                file.write_all(b"\n")?;
                file.sync_all()?;
            }
            TailRepair::TruncateTo(valid_len) => repair_torn_tail(&path, valid_len)?,
        }
        let document = state.document(header.clone());
        Ok((
            Self::from_parts(
                path,
                header,
                state,
                modified_at,
                SessionPersistence::Materialized,
            ),
            document,
        ))
    }

    fn from_parts(
        path: PathBuf,
        header: SessionHeader,
        state: SessionState,
        modified_at: f64,
        persistence: SessionPersistence,
    ) -> Self {
        let metadata = JsonlSessionMetadata {
            id: header.id.clone(),
            created_at: header.created_at,
            cwd: header.cwd.clone(),
            path: path.clone(),
            modified_at,
            source_format: SESSION_SCHEMA_VERSION,
            parent_session_id: header.parent_session_id.clone(),
            legacy_parent_session_path: header.legacy_parent_session_path.clone(),
            metadata: header.metadata.clone(),
        };
        Self {
            inner: Arc::new(SessionLogInner {
                path,
                header,
                metadata,
                state: Mutex::new(state),
                persistence: Mutex::new(persistence),
            }),
        }
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    pub fn header(&self) -> SessionHeader {
        self.inner.header.clone()
    }

    pub fn metadata(&self) -> Result<JsonlSessionMetadata, SessionError> {
        let mut metadata = self.inner.metadata.clone();
        if self.is_materialized() {
            metadata.modified_at = file_modified_at(&self.inner.path)?;
        }
        Ok(metadata)
    }

    pub fn is_materialized(&self) -> bool {
        matches!(
            *self
                .inner
                .persistence
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            SessionPersistence::Materialized
        )
    }

    /// Writes the header and all mutations accumulated by a deferred log.
    /// Returns `true` when this call created the JSONL file.
    pub fn materialize(&self) -> Result<bool, SessionError> {
        let mut persistence = self
            .inner
            .persistence
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let SessionPersistence::Deferred { encoded_mutations } = &*persistence else {
            return Ok(false);
        };

        if let Some(parent) = self.inner.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut created = false;
        let result = (|| {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&self.inner.path)?;
            created = true;
            write_json_line(&mut file, &self.inner.header, 1)?;
            file.write_all(encoded_mutations)?;
            file.sync_all()?;
            Ok::<(), SessionError>(())
        })();
        if let Err(error) = result {
            if created {
                let _ = std::fs::remove_file(&self.inner.path);
            }
            return Err(error);
        }
        *persistence = SessionPersistence::Materialized;
        Ok(true)
    }

    pub fn load(&self) -> Result<SessionDocument, SessionError> {
        Ok(self.state().document(self.inner.header.clone()))
    }

    /// Exports the active main-lane branch as a standalone v4 JSONL session.
    ///
    /// Runtime-only records and abandoned branches are intentionally omitted,
    /// matching Pi's portable `/export <file>.jsonl` behavior. The source log
    /// remains open and unchanged; the destination is replaced atomically.
    pub fn export_branch(&self, path: impl Into<PathBuf>) -> Result<PathBuf, SessionError> {
        if !self.is_materialized() {
            return Err(SessionError::Storage(
                "nothing to export yet; wait for the first assistant response".to_string(),
            ));
        }

        let path = path.into();
        if comparable_path(&path) == comparable_path(&self.inner.path) {
            return Err(SessionError::Storage(
                "cannot export over the active session file".to_string(),
            ));
        }
        if path.is_dir() {
            return Err(SessionError::Storage(format!(
                "export destination is a directory: {}",
                path.display()
            )));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mutations = self.state().create_fork_mutations(&ForkOptions::Branch {
            entry_id: None,
            position: Some(crate::ForkPosition::At),
        })?;
        let mut header = self.inner.header.clone();
        header.created_at = now_ms();
        header.parent_session_id = None;
        header.legacy_parent_session_path = None;

        let temporary = sibling_transaction_path(&path, "export");
        let backup = path
            .exists()
            .then(|| sibling_transaction_path(&path, "backup"));
        let result = (|| {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            write_json_line(&mut file, &header, 1)?;
            for (index, mutation) in mutations.iter().enumerate() {
                write_json_line(&mut file, mutation, index + 2)?;
            }
            file.sync_all()?;
            drop(file);

            if let Some(backup) = &backup {
                std::fs::rename(&path, backup)?;
            }
            if let Err(error) = std::fs::rename(&temporary, &path) {
                if let Some(backup) = &backup {
                    let _ = std::fs::rename(backup, &path);
                }
                return Err(error.into());
            }
            if let Some(backup) = &backup {
                let _ = std::fs::remove_file(backup);
            }
            Ok::<(), SessionError>(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result?;
        Ok(path)
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
        let mutation = SessionMutation::Lane {
            seq: state.next_sequence(),
            lane,
            leaf_id: at.map(str::to_string),
        };
        self.commit(&mut state, mutation)
    }

    pub fn move_lane(&self, lane: &str, to: Option<&str>) -> Result<(), SessionError> {
        let mut state = self.state();
        state.require_lane(lane)?;
        state.validate_target(to)?;
        let mutation = SessionMutation::Lane {
            seq: state.next_sequence(),
            lane: lane.to_string(),
            leaf_id: to.map(str::to_string),
        };
        self.commit(&mut state, mutation)
    }

    /// Compatibility spelling for moving the main lane.
    pub fn branch(&self, leaf_id: Option<&str>) -> Result<(), SessionError> {
        self.move_lane(MAIN_LANE, leaf_id)
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
        let mutation = SessionMutation::Entry {
            lane: Some(lane.to_string()),
            record: record.clone(),
        };
        self.commit(&mut state, mutation)?;
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
        self.append(SessionEntry::Custom(crate::CustomEntry {
            custom_type: custom_type.into(),
            data,
        }))
    }

    pub fn append_batch(
        &self,
        entries: impl IntoIterator<Item = SessionEntry>,
    ) -> Result<Vec<String>, SessionError> {
        let mut state = self.state();
        let mut staged = state.clone();
        let mut mutations = Vec::new();
        let mut ids = Vec::new();
        for entry in entries {
            let id = next_unique_id("entry");
            let record = SessionRecord {
                id: id.clone(),
                seq: staged.next_sequence(),
                parent_id: staged.require_lane(MAIN_LANE)?,
                timestamp_ms: now_ms(),
                entry,
            };
            let mutation = SessionMutation::Entry {
                lane: Some(MAIN_LANE.to_string()),
                record,
            };
            staged.apply_mutation(mutation.clone())?;
            mutations.push(mutation);
            ids.push(id);
        }
        self.append_mutations(&mutations)?;
        *state = staged;
        Ok(ids)
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
        let mutation = SessionMutation::Record {
            record: record.clone(),
        };
        self.commit(&mut state, mutation)?;
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
        let mutation = SessionMutation::Fact {
            seq: state.next_sequence(),
            fact: SessionFact::Name { name },
        };
        self.commit(&mut state, mutation)
    }

    pub fn label(&self, target_id: &str) -> Option<String> {
        self.state().label(target_id)
    }

    pub fn set_label(&self, target_id: &str, label: Option<String>) -> Result<(), SessionError> {
        let mut state = self.state();
        state.validate_target(Some(target_id))?;
        let mutation = SessionMutation::Fact {
            seq: state.next_sequence(),
            fact: SessionFact::Label {
                target_id: target_id.to_string(),
                label,
            },
        };
        self.commit(&mut state, mutation)
    }

    pub fn stats(&self) -> SessionStats {
        self.state().stats()
    }

    pub fn fork(
        &self,
        path: impl Into<PathBuf>,
        header: SessionHeader,
        options: &ForkOptions,
    ) -> Result<Self, SessionError> {
        if !self.is_materialized() {
            return Err(SessionError::Storage(
                "session has not been saved yet; wait for the first assistant response".to_string(),
            ));
        }
        validate_header(&header)?;
        let path = path.into();
        if path.exists() {
            return Err(SessionError::AlreadyExists(path.display().to_string()));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mutations = self.state().create_fork_mutations(options)?;
        let mut validation_state = SessionState::default();
        for mutation in &mutations {
            validation_state.apply_mutation(mutation.clone())?;
        }
        let temporary = sibling_temporary_path(&path);
        let result = (|| {
            let mut file = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&temporary)?;
            write_json_line(&mut file, &header, 1)?;
            for (index, mutation) in mutations.iter().enumerate() {
                write_json_line(&mut file, mutation, index + 2)?;
            }
            file.sync_all()?;
            std::fs::rename(&temporary, &path)?;
            Ok::<(), SessionError>(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result?;
        let (log, _) = Self::open(path)?;
        Ok(log)
    }

    fn state(&self) -> MutexGuard<'_, SessionState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn commit(
        &self,
        state: &mut MutexGuard<'_, SessionState>,
        mutation: SessionMutation,
    ) -> Result<(), SessionError> {
        validate_mutation_payload(&mutation)?;
        // Validate on a clone before the durable append, so rejected mutations
        // never poison the log.
        let mut staged = (**state).clone();
        staged.apply_mutation(mutation.clone())?;
        self.append_mutations(std::slice::from_ref(&mutation))?;
        **state = staged;
        Ok(())
    }

    fn append_mutations(&self, mutations: &[SessionMutation]) -> Result<(), SessionError> {
        if mutations.is_empty() {
            return Ok(());
        }
        let mut encoded = Vec::new();
        for (index, mutation) in mutations.iter().enumerate() {
            validate_mutation_payload(mutation)?;
            let line = encode_json_line(mutation, index + 2)?;
            encoded.extend_from_slice(&line);
        }
        let mut persistence = self
            .inner
            .persistence
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &mut *persistence {
            SessionPersistence::Materialized => {
                let mut file = OpenOptions::new().append(true).open(&self.inner.path)?;
                file.write_all(&encoded)?;
                file.sync_data()?;
            }
            SessionPersistence::Deferred { encoded_mutations } => {
                encoded_mutations.extend_from_slice(&encoded);
            }
        }
        Ok(())
    }
}

enum TailRepair {
    None,
    AppendNewline,
    TruncateTo(usize),
}

fn load_file(path: &Path) -> Result<(SessionHeader, SessionState, TailRepair), SessionError> {
    let bytes = std::fs::read(path)?;
    if bytes.is_empty() {
        return Err(SessionError::MissingHeader);
    }
    let mut lines = bytes.split(|byte| *byte == b'\n').collect::<Vec<_>>();
    let terminated = bytes.ends_with(b"\n");
    if terminated {
        lines.pop();
    }
    let Some(header_line) = lines.first() else {
        return Err(SessionError::MissingHeader);
    };
    let header_value: Value =
        serde_json::from_slice(header_line).map_err(|error| SessionError::InvalidJson {
            line: 1,
            message: error.to_string(),
        })?;
    if !header_value.is_object() {
        return Err(SessionError::MissingHeader);
    }
    validate_header_json_shape(&header_value, 1)?;
    let header: SessionHeader =
        serde_json::from_value(header_value).map_err(|error| SessionError::InvalidJson {
            line: 1,
            message: error.to_string(),
        })?;
    validate_header(&header)?;

    let mut state = SessionState::default();
    let mut offset = header_line.len() + usize::from(lines.len() > 1 || terminated);
    for (index, line) in lines.iter().enumerate().skip(1) {
        let line_number = index + 1;
        let is_last = index == lines.len() - 1;
        let value = match serde_json::from_slice::<Value>(line) {
            Ok(value) => value,
            Err(_error) if is_last => {
                return Ok((header, state, TailRepair::TruncateTo(offset)));
            }
            Err(error) => {
                return Err(SessionError::InvalidJson {
                    line: line_number,
                    message: error.to_string(),
                });
            }
        };
        if !value.is_object() {
            return Err(SessionError::InvalidJson {
                line: line_number,
                message: "session mutation is not a JSON object".to_string(),
            });
        }
        validate_mutation_json_shape(&value, line_number)?;
        let mutation: SessionMutation =
            serde_json::from_value(value).map_err(|error| SessionError::InvalidJson {
                line: line_number,
                message: error.to_string(),
            })?;
        validate_mutation_shape(&mutation, line_number)?;
        state
            .apply_mutation(mutation)
            .map_err(|error| SessionError::InvalidJson {
                line: line_number,
                message: error.to_string(),
            })?;
        offset = offset.saturating_add(line.len() + usize::from(!is_last || terminated));
    }
    let repair = if terminated {
        TailRepair::None
    } else {
        TailRepair::AppendNewline
    };
    Ok((header, state, repair))
}

pub(crate) fn validate_header(header: &SessionHeader) -> Result<(), SessionError> {
    if header.version != SESSION_SCHEMA_VERSION {
        return Err(SessionError::UnsupportedSchema(header.version));
    }
    if header.created_at < 0
        || u64::try_from(header.created_at).is_ok_and(|value| value > MAX_SAFE_INTEGER)
    {
        return Err(SessionError::InvalidPayload(
            "header createdAt must be a non-negative safe integer".to_string(),
        ));
    }
    if header.parent_session_id.is_some() && header.legacy_parent_session_path.is_some() {
        return Err(SessionError::InvalidPayload(
            "header cannot contain both parentSessionId and legacyParentSessionPath".to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn validate_header_json_shape(value: &Value, line: usize) -> Result<(), SessionError> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid_json_shape(line, "is not a header"))?;
    if object.get("kind").and_then(Value::as_str) != Some("header") {
        return Err(invalid_json_shape(line, "is not a header"));
    }
    if object.get("version").and_then(Value::as_u64) != Some(u64::from(SESSION_SCHEMA_VERSION)) {
        return Err(invalid_json_shape(line, "has unsupported session version"));
    }
    require_json_string(object, "id", line)?;
    require_safe_integer(object.get("createdAt"), line, "createdAt", true)?;
    require_json_string(object, "cwd", line)?;
    let parent = optional_json_string(object, "parentSessionId", line)?;
    let legacy_parent = optional_json_string(object, "legacyParentSessionPath", line)?;
    if parent && legacy_parent {
        return Err(invalid_json_shape(
            line,
            "has both parentSessionId and legacyParentSessionPath",
        ));
    }
    if let Some(metadata) = object.get("metadata")
        && !metadata.is_object()
    {
        return Err(invalid_json_shape(line, "has invalid metadata"));
    }
    Ok(())
}

fn validate_mutation_json_shape(value: &Value, line: usize) -> Result<(), SessionError> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid_json_shape(line, "is not a JSON object"))?;
    require_safe_integer(object.get("seq"), line, "seq", false)?;
    match object.get("kind").and_then(Value::as_str) {
        Some("entry") => {
            optional_json_string(object, "lane", line)?;
            require_json_string(object, "id", line)?;
            let entry_type = require_json_string(object, "type", line)?;
            if !matches!(
                entry_type,
                "message"
                    | "custom_message"
                    | "model_change"
                    | "thinking_level_change"
                    | "active_tools_change"
                    | "compaction"
                    | "branch_summary"
                    | "custom"
            ) {
                return Err(invalid_json_shape(
                    line,
                    format!("has unknown entry type {entry_type}"),
                ));
            }
            require_nullable_json_string(object, "parentId", line)?;
            require_safe_integer(object.get("timestamp"), line, "timestamp", true)?;
            if matches!(entry_type, "custom" | "custom_message") {
                require_json_string(object, "customType", line)?;
            }
        }
        Some("record") => {
            require_json_string(object, "id", line)?;
            require_json_string(object, "lane", line)?;
            let record_type = require_json_string(object, "type", line)?;
            if !matches!(
                record_type,
                "operation_started"
                    | "abort_requested"
                    | "operation_finished"
                    | "step_attempt"
                    | "tool_started"
                    | "queue_enqueued"
                    | "queue_cancelled"
                    | "write_deferred"
                    | "usage"
            ) {
                return Err(invalid_json_shape(
                    line,
                    format!("has unknown record type {record_type}"),
                ));
            }
            require_safe_integer(object.get("timestamp"), line, "timestamp", true)?;
            if record_type == "operation_started" {
                let intent = object
                    .get("intent")
                    .and_then(Value::as_object)
                    .ok_or_else(|| invalid_json_shape(line, "has invalid intent"))?;
                let kind = require_json_string(intent, "kind", line)?;
                if !matches!(kind, "run" | "compaction" | "navigation") {
                    return Err(invalid_json_shape(
                        line,
                        format!("has unknown operation kind {kind}"),
                    ));
                }
            }
            if record_type == "operation_finished" {
                require_json_string(object, "runId", line)?;
            }
        }
        Some("lane") => {
            require_json_string(object, "lane", line)?;
            require_nullable_json_string(object, "leafId", line)?;
        }
        Some("fact") => match object.get("fact").and_then(Value::as_str) {
            Some("name") => {
                optional_json_string(object, "name", line)?;
            }
            Some("label") => {
                require_json_string(object, "targetId", line)?;
                optional_json_string(object, "label", line)?;
            }
            _ => return Err(invalid_json_shape(line, "has unknown fact type")),
        },
        _ => return Err(invalid_json_shape(line, "has unknown mutation kind")),
    }
    Ok(())
}

fn require_json_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    field: &str,
    line: usize,
) -> Result<&'a str, SessionError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_json_shape(line, format!("has invalid {field}")))
}

/// Returns true only when the optional property is present and valid.
fn optional_json_string(
    object: &serde_json::Map<String, Value>,
    field: &str,
    line: usize,
) -> Result<bool, SessionError> {
    match object.get(field) {
        None => Ok(false),
        Some(Value::String(_)) => Ok(true),
        Some(_) => Err(invalid_json_shape(line, format!("has invalid {field}"))),
    }
}

fn require_nullable_json_string(
    object: &serde_json::Map<String, Value>,
    field: &str,
    line: usize,
) -> Result<(), SessionError> {
    match object.get(field) {
        Some(Value::Null | Value::String(_)) => Ok(()),
        _ => Err(invalid_json_shape(line, format!("has invalid {field}"))),
    }
}

fn require_safe_integer(
    value: Option<&Value>,
    line: usize,
    field: &str,
    allow_zero: bool,
) -> Result<(), SessionError> {
    let valid = value
        .and_then(Value::as_u64)
        .is_some_and(|value| value <= MAX_SAFE_INTEGER && (allow_zero || value > 0));
    if valid {
        Ok(())
    } else {
        Err(invalid_json_shape(line, format!("has invalid {field}")))
    }
}

fn invalid_json_shape(line: usize, message: impl Into<String>) -> SessionError {
    SessionError::InvalidJson {
        line,
        message: message.into(),
    }
}

fn validate_mutation_shape(mutation: &SessionMutation, line: usize) -> Result<(), SessionError> {
    if mutation.seq() == 0 || mutation.seq() > MAX_SAFE_INTEGER {
        return Err(SessionError::InvalidJson {
            line,
            message: "session mutation has invalid seq".to_string(),
        });
    }
    let timestamp = match mutation {
        SessionMutation::Entry { record, .. } => Some(record.timestamp_ms),
        SessionMutation::Record { record } => Some(record.timestamp_ms),
        SessionMutation::Lane { .. } | SessionMutation::Fact { .. } => None,
    };
    if timestamp.is_some_and(|timestamp| {
        timestamp < 0 || u64::try_from(timestamp).is_ok_and(|value| value > MAX_SAFE_INTEGER)
    }) {
        return Err(SessionError::InvalidJson {
            line,
            message: "session mutation has invalid timestamp".to_string(),
        });
    }
    Ok(())
}

fn encode_json_line<T: Serialize>(value: &T, line: usize) -> Result<Vec<u8>, SessionError> {
    let mut encoded = serde_json::to_vec(value).map_err(|error| SessionError::InvalidJson {
        line,
        message: error.to_string(),
    })?;
    encoded.push(b'\n');
    Ok(encoded)
}

fn write_json_line<T: Serialize>(
    writer: &mut impl Write,
    value: &T,
    line: usize,
) -> Result<(), SessionError> {
    writer.write_all(&encode_json_line(value, line)?)?;
    Ok(())
}

fn repair_torn_tail(path: &Path, valid_len: usize) -> Result<(), SessionError> {
    let bytes = std::fs::read(path)?;
    let temporary = sibling_temporary_path(path);
    let result = (|| {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(&bytes[..valid_len])?;
        if valid_len > 0 && bytes.get(valid_len.wrapping_sub(1)) != Some(&b'\n') {
            file.write_all(b"\n")?;
        }
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        Ok::<(), SessionError>(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn sibling_temporary_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map_or_else(|| "session".into(), |name| name.to_os_string());
    name.push(".tmp");
    path.with_file_name(name)
}

fn sibling_transaction_path(path: &Path, purpose: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map_or_else(|| "session".into(), |name| name.to_os_string());
    name.push(format!(".{purpose}-{}.tmp", uuid::Uuid::now_v7()));
    path.with_file_name(name)
}

fn comparable_path(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    match (absolute.parent(), absolute.file_name()) {
        (Some(parent), Some(file_name)) => std::fs::canonicalize(parent)
            .map(|parent| parent.join(file_name))
            .unwrap_or(absolute),
        _ => absolute,
    }
}

fn file_modified_at(path: &Path) -> Result<f64, SessionError> {
    Ok(std::fs::metadata(path)?
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0.0, |duration| duration.as_secs_f64() * 1_000.0))
}

#[cfg(test)]
mod tests {
    use pi_core::{Message, UserMessage};

    use super::*;
    use crate::{EntryOrder, HeaderKind, SessionEntryType};

    fn header() -> SessionHeader {
        SessionHeader {
            kind: HeaderKind::Header,
            version: 4,
            id: "session".to_string(),
            created_at: 1,
            cwd: "/project".into(),
            parent_session_id: None,
            legacy_parent_session_path: None,
            metadata: None,
        }
    }

    #[test]
    fn deferred_log_materializes_all_staged_mutations_once() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested/session.jsonl");
        let log = SessionLog::create_deferred(&path, header()).unwrap();
        let id = log
            .append_message(Message::User(UserMessage::text("hello", 1)))
            .unwrap();
        log.set_label(&id, Some("first".to_string())).unwrap();

        assert!(!path.exists());
        assert!(!log.is_materialized());
        assert_eq!(log.load().unwrap().messages().len(), 1);

        assert!(log.materialize().unwrap());
        assert!(path.exists());
        assert!(log.is_materialized());
        assert!(!log.materialize().unwrap());
        log.append_message(Message::User(UserMessage::text("after", 2)))
            .unwrap();

        let (_, reopened) = SessionLog::open(&path).unwrap();
        assert_eq!(reopened.messages().len(), 2);
        assert_eq!(reopened.labels.get(&id).map(String::as_str), Some("first"));
    }

    #[test]
    fn deferred_materialization_never_overwrites_a_racing_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let log = SessionLog::create_deferred(&path, header()).unwrap();
        log.append_message(Message::User(UserMessage::text("staged", 1)))
            .unwrap();
        std::fs::write(&path, "sentinel\n").unwrap();

        assert!(log.materialize().is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "sentinel\n");
        assert!(!log.is_materialized());
        assert_eq!(log.load().unwrap().messages().len(), 1);
    }

    #[test]
    fn deferred_log_cannot_be_forked_before_it_is_saved() {
        let directory = tempfile::tempdir().unwrap();
        let log =
            SessionLog::create_deferred(directory.path().join("source.jsonl"), header()).unwrap();
        let error = match log.fork(
            directory.path().join("fork.jsonl"),
            SessionHeader {
                id: "fork".to_string(),
                ..header()
            },
            &ForkOptions::default(),
        ) {
            Ok(_) => panic!("expected an unsaved session to reject forking"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("first assistant response"));
    }

    #[test]
    fn writes_exact_v4_shape_and_replays_shared_sequence() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let log = SessionLog::create(&path, header()).unwrap();
        let id = log
            .append_message(Message::User(UserMessage::text("hello", 1)))
            .unwrap();
        log.set_label(&id, Some("first".to_string())).unwrap();

        let lines = std::fs::read_to_string(&path).unwrap();
        let values = lines
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(values[0]["kind"], "header");
        assert_eq!(values[0]["version"], 4);
        assert_eq!(values[1]["kind"], "entry");
        assert_eq!(values[1]["seq"], 1);
        assert_eq!(values[1]["type"], "message");
        assert_eq!(values[1]["message"]["timestamp"], 1);
        assert_eq!(values[2]["kind"], "fact");
        assert_eq!(values[2]["seq"], 2);

        let (_, document) = SessionLog::open(&path).unwrap();
        assert_eq!(document.stats.message_count, 1);
        assert_eq!(document.labels.get(&id).map(String::as_str), Some("first"));
    }

    #[test]
    fn exports_only_the_active_branch_as_a_portable_v4_session() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("source.jsonl");
        let export_path = directory.path().join("portable.jsonl");
        let mut source_header = header();
        source_header.parent_session_id = Some("parent-session".to_string());
        let log = SessionLog::create(&source_path, source_header).unwrap();
        let root = log
            .append_message(Message::User(UserMessage::text("root", 1)))
            .unwrap();
        let abandoned = log
            .append_message(Message::User(UserMessage::text("abandoned", 2)))
            .unwrap();
        log.branch(Some(&root)).unwrap();
        let active = log
            .append_message(Message::User(UserMessage::text("active", 3)))
            .unwrap();
        log.set_name(Some("portable name".to_string())).unwrap();
        log.set_label(&active, Some("chosen".to_string())).unwrap();
        std::fs::write(&export_path, "replace me").unwrap();

        assert_eq!(log.export_branch(&export_path).unwrap(), export_path);

        let (_, document) = SessionLog::open(&export_path).unwrap();
        assert_eq!(document.header.id, log.header().id);
        assert_eq!(document.header.parent_session_id, None);
        assert_eq!(document.name.as_deref(), Some("portable name"));
        assert!(document.entries.iter().any(|entry| entry.id == root));
        assert!(document.entries.iter().any(|entry| entry.id == active));
        assert!(!document.entries.iter().any(|entry| entry.id == abandoned));
        assert_eq!(
            document.labels.get(&active).map(String::as_str),
            Some("chosen")
        );
        assert_eq!(document.branch().unwrap().len(), 2);
        assert!(source_path.exists());
    }

    #[test]
    fn export_rejects_the_active_session_path() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let log = SessionLog::create(&path, header()).unwrap();

        let error = log.export_branch(&path).unwrap_err();

        assert!(error.to_string().contains("active session file"));
        assert!(SessionLog::open(path).is_ok());
    }

    #[test]
    fn repairs_only_a_syntactically_torn_final_append() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let log = SessionLog::create(&path, header()).unwrap();
        log.append_message(Message::User(UserMessage::text("before", 1)))
            .unwrap();
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"kind\":\"entry\"")
            .unwrap();

        let (reopened, document) = SessionLog::open(&path).unwrap();
        assert_eq!(document.messages().len(), 1);
        reopened
            .append_message(Message::User(UserMessage::text("after", 2)))
            .unwrap();
        assert_eq!(reopened.load().unwrap().messages().len(), 2);
    }

    #[test]
    fn preserves_agent_message_wire_extensions_across_jsonl_replay() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let log = SessionLog::create(&path, header()).unwrap();
        let wire = serde_json::json!({
            "role": "user",
            "content": "compact input",
            "timestamp": 9,
            "futureField": {"keep": true}
        });
        log.append_message(AgentMessage::custom(wire.clone()).unwrap())
            .unwrap();

        let (_, document) = SessionLog::open(&path).unwrap();
        assert_eq!(serde_json::to_value(&document.messages()[0]).unwrap(), wire);
    }

    #[test]
    fn rejects_optional_nulls_that_the_typescript_codec_rejects() {
        let directory = tempfile::tempdir().unwrap();
        let header_path = directory.path().join("invalid-header.jsonl");
        let invalid_header = serde_json::json!({
            "kind": "header",
            "version": 4,
            "id": "invalid-header",
            "createdAt": 1,
            "cwd": "/project",
            "parentSessionId": null
        });
        std::fs::write(&header_path, format!("{invalid_header}\n")).unwrap();
        let error = match SessionLog::open(&header_path) {
            Ok(_) => panic!("expected invalid header"),
            Err(error) => error,
        };
        assert_eq!(error.code(), crate::SessionErrorCode::InvalidEntry);

        let fact_path = directory.path().join("invalid-fact.jsonl");
        let valid_header = serde_json::to_string(&header()).unwrap();
        let invalid_fact = serde_json::json!({
            "kind": "fact",
            "seq": 1,
            "fact": "name",
            "name": null
        });
        let original = format!("{valid_header}\n{invalid_fact}\n");
        std::fs::write(&fact_path, &original).unwrap();
        let error = match SessionLog::open(&fact_path) {
            Ok(_) => panic!("expected invalid fact"),
            Err(error) => error,
        };
        assert_eq!(error.code(), crate::SessionErrorCode::InvalidEntry);
        assert_eq!(std::fs::read_to_string(fact_path).unwrap(), original);
    }

    #[test]
    fn queries_and_persisted_lane_moves_follow_v4_rules() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let log = SessionLog::create(&path, header()).unwrap();
        let first = log
            .append_message(Message::User(UserMessage::text("first", 1)))
            .unwrap();
        log.append_message(Message::User(UserMessage::text("abandoned", 2)))
            .unwrap();
        log.move_lane(MAIN_LANE, Some(&first)).unwrap();
        log.append_message(Message::User(UserMessage::text("replacement", 3)))
            .unwrap();

        let branch = log
            .find_entries_on_branch(&BranchQuery {
                entries: EntryQuery {
                    entry_type: Some(SessionEntryType::Message),
                    order: EntryOrder::OldestFirst,
                    ..EntryQuery::default()
                },
                ..BranchQuery::default()
            })
            .unwrap();
        assert_eq!(branch.len(), 2);
        let (_, reopened) = SessionLog::open(&path).unwrap();
        assert_eq!(reopened.context().unwrap().messages.len(), 2);
    }
}
