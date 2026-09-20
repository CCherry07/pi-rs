use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use serde_json::{Value, value::RawValue};

use super::state::SessionState;
use super::validation::{
    validate_mutation_payload, validate_new_lane_record, validate_provisioned_entry,
};
use crate::{
    AgentMessage, BranchQuery, EntryQuery, ForkOptions, JsonlSessionMetadata, LanePointer,
    LaneRecord, LaneRecordEntry, LogItem, MAIN_LANE, MessageEntry, NewLaneRecord, ProvisionedEntry,
    RecordQuery, SESSION_SCHEMA_VERSION, SessionContext, SessionContextBuildOptions,
    SessionDocument, SessionEntry, SessionError, SessionFact, SessionHeader, SessionMutation,
    SessionRecord, SessionStats, SessionWireError, build_session_context, next_unique_id, now_ms,
};

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const PARALLEL_REPLAY_MIN_BYTES: usize = 1_048_576;
const PARALLEL_REPLAY_MIN_MUTATIONS: usize = 2_048;
const PARALLEL_REPLAY_MAX_WORKERS: usize = 8;

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
        let log = Self::open_handle(path)?;
        let document = log.load()?;
        Ok((log, document))
    }

    /// Opens and fully validates a persisted journal without cloning an
    /// immediate [`SessionDocument`] snapshot. Frontends that need session
    /// metadata before constructing the runtime can retain this handle and
    /// hand it to the session manager, avoiding a second JSONL replay.
    pub fn open_handle(path: impl Into<PathBuf>) -> Result<Self, SessionError> {
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
        Ok(Self::from_parts(
            path,
            header,
            state,
            modified_at,
            SessionPersistence::Materialized,
        ))
    }

    /// Reads a coherent v4 document without modifying the source file.
    ///
    /// A syntactically torn final append is ignored in the returned snapshot,
    /// but unlike [`SessionLog::open`] this method never appends a newline or
    /// truncates the source. This makes it safe for derived-index scans that
    /// may observe a session while another process is appending to it.
    pub fn read(path: impl AsRef<Path>) -> Result<SessionDocument, SessionError> {
        let (header, state, _repair) = load_file(path.as_ref())?;
        Ok(state.document(header))
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

    pub fn id(&self) -> &str {
        &self.inner.header.id
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

    /// Returns a revision-stable immutable document shared by all readers.
    /// The first read after a mutation materializes the document; subsequent
    /// reads at the same revision clone only the [`Arc`].
    pub fn shared_document(&self) -> Result<Arc<SessionDocument>, SessionError> {
        Ok(self.state().shared_document(self.inner.header.clone()))
    }

    #[cfg(test)]
    pub(crate) fn has_cached_document(&self) -> bool {
        self.state().has_cached_document()
    }

    /// Captures the active main-lane branch and its name/labels as v4 mutations.
    ///
    /// Sequences start at one; runtime records, other lanes, and abandoned
    /// branches are omitted. This read-only snapshot also works for an
    /// unmaterialized log and does not select a destination or rewrite its header.
    pub fn main_branch_snapshot(&self) -> Result<Vec<SessionMutation>, SessionError> {
        self.state().create_main_branch_snapshot_mutations()
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
        Ok(self.append_session_record_to_lane(entry, lane)?.id)
    }

    /// Appends an entry and returns the authoritative tree record that was
    /// committed. Frontend wire adapters use this to preserve Pi entry IDs,
    /// parents, and timestamps without re-reading mutable session state.
    pub fn append_session_record_to_lane(
        &self,
        entry: SessionEntry,
        lane: &str,
    ) -> Result<SessionRecord, SessionError> {
        self.append_entry(
            ProvisionedEntry {
                id: next_unique_id("entry"),
                entry,
            },
            lane,
        )
    }

    pub fn append(&self, entry: SessionEntry) -> Result<String, SessionError> {
        self.append_to_lane(entry, MAIN_LANE)
    }

    pub fn append_session_record(
        &self,
        entry: SessionEntry,
    ) -> Result<SessionRecord, SessionError> {
        self.append_session_record_to_lane(entry, MAIN_LANE)
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
        let mut mutations = Vec::new();
        let mut ids = Vec::new();
        let mut parent_id = state.require_lane(MAIN_LANE)?;
        let mut next_sequence = state.next_sequence();
        for entry in entries {
            let id = next_unique_id("entry");
            let record = SessionRecord {
                id: id.clone(),
                seq: next_sequence,
                parent_id,
                timestamp_ms: now_ms(),
                entry,
            };
            let mutation = SessionMutation::Entry {
                lane: Some(MAIN_LANE.to_string()),
                record,
            };
            mutations.push(mutation);
            parent_id = Some(id.clone());
            next_sequence = next_sequence.saturating_add(1);
            ids.push(id);
        }
        self.commit_mutations(&mut state, mutations)?;
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

    /// Reads the active main branch directly from the journal's structural
    /// index without materializing records, facts, or the full mutation log.
    pub fn branch_entries(&self) -> Result<Vec<SessionRecord>, SessionError> {
        self.state().branch_entries_for_lane(MAIN_LANE)
    }

    pub fn branch_entries_at(
        &self,
        leaf_id: Option<&str>,
    ) -> Result<Vec<SessionRecord>, SessionError> {
        self.state().branch_entries_at(leaf_id)
    }

    pub fn context(&self) -> Result<SessionContext, SessionError> {
        self.context_with_options(&SessionContextBuildOptions::default())
    }

    pub fn context_with_options(
        &self,
        options: &SessionContextBuildOptions,
    ) -> Result<SessionContext, SessionError> {
        if options.entry_transforms.is_empty() && options.entry_projectors.is_empty() {
            return self.state().default_context_for_lane(MAIN_LANE);
        }
        let entries = self.branch_entries()?;
        Ok(build_session_context(&entries, options))
    }

    pub fn context_at_with_options(
        &self,
        leaf_id: Option<&str>,
        options: &SessionContextBuildOptions,
    ) -> Result<SessionContext, SessionError> {
        if options.entry_transforms.is_empty() && options.entry_projectors.is_empty() {
            return self.state().default_context_at(leaf_id);
        }
        let entries = self.branch_entries_at(leaf_id)?;
        Ok(build_session_context(&entries, options))
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
        state.validate_mutation(&mutation)?;
        self.append_mutations(std::slice::from_ref(&mutation))?;
        state.apply_validated_mutation(mutation);
        Ok(())
    }

    fn commit_mutations(
        &self,
        state: &mut MutexGuard<'_, SessionState>,
        mutations: Vec<SessionMutation>,
    ) -> Result<(), SessionError> {
        for mutation in &mutations {
            validate_mutation_payload(mutation)?;
        }
        state.validate_mutations(&mutations)?;
        self.append_mutations(&mutations)?;
        for mutation in mutations {
            state.apply_validated_mutation(mutation);
        }
        Ok(())
    }

    fn append_mutations(&self, mutations: &[SessionMutation]) -> Result<(), SessionError> {
        if mutations.is_empty() {
            return Ok(());
        }
        let mut encoded = Vec::new();
        for (index, mutation) in mutations.iter().enumerate() {
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

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum EntryMutationKind {
    Entry,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum MessageEntryKind {
    Message,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawMessageMutation<'a> {
    #[serde(rename = "kind")]
    _kind: EntryMutationKind,
    seq: u64,
    #[serde(
        default,
        deserialize_with = "crate::types::strict_optional::deserialize"
    )]
    lane: Option<String>,
    id: String,
    #[serde(
        rename = "parentId",
        deserialize_with = "crate::types::required_nullable::deserialize"
    )]
    parent_id: Option<String>,
    #[serde(rename = "timestamp", with = "crate::types::iso_timestamp_ms")]
    timestamp_ms: i64,
    #[serde(rename = "type")]
    _entry_type: MessageEntryKind,
    #[serde(borrow)]
    message: &'a RawValue,
    #[serde(default)]
    terminate: bool,
}

impl RawMessageMutation<'_> {
    fn into_mutation(self, source: Arc<Vec<u8>>) -> Result<SessionMutation, SessionWireError> {
        let source_start = source.as_ptr() as usize;
        let message_start = self.message.get().as_ptr() as usize;
        let start = message_start.checked_sub(source_start).ok_or_else(|| {
            SessionWireError::InvalidPayload(
                "agent message is outside the session source buffer".to_string(),
            )
        })?;
        let end = start.checked_add(self.message.get().len()).ok_or_else(|| {
            SessionWireError::InvalidPayload("agent message source range overflowed".to_string())
        })?;
        if end > source.len() {
            return Err(SessionWireError::InvalidPayload(
                "agent message is outside the session source buffer".to_string(),
            ));
        }
        Ok(SessionMutation::Entry {
            lane: self.lane,
            record: SessionRecord {
                id: self.id,
                seq: self.seq,
                parent_id: self.parent_id,
                timestamp_ms: self.timestamp_ms,
                entry: SessionEntry::Message(MessageEntry {
                    message: AgentMessage::from_shared_raw_json(source, start..end)?,
                    terminate: self.terminate,
                }),
            },
        })
    }
}

enum MutationDecodeError {
    Json(serde_json::Error),
    Wire(SessionWireError),
}

impl MutationDecodeError {
    fn is_torn_json(&self) -> bool {
        matches!(self, Self::Json(error) if error.is_syntax() || error.is_eof())
    }

    fn message(self) -> String {
        match self {
            Self::Json(error) => error.to_string(),
            Self::Wire(error) => error.to_string(),
        }
    }
}

fn decode_mutation(
    line: &[u8],
    source: &Arc<Vec<u8>>,
) -> Result<SessionMutation, MutationDecodeError> {
    // serde emits the internally tagged `kind` first, as does current Pi's
    // JSONL writer. Route non-entry mutations directly to the generic decoder
    // so operation-heavy sessions do not parse every record twice. Files with
    // alternate valid whitespace or field ordering retain the generic path.
    if line.starts_with(br#"{"kind":"entry","#) {
        match serde_json::from_slice::<RawMessageMutation<'_>>(line) {
            Ok(message) => message
                .into_mutation(Arc::clone(source))
                .map_err(MutationDecodeError::Wire),
            Err(_) => serde_json::from_slice(line).map_err(MutationDecodeError::Json),
        }
    } else {
        serde_json::from_slice(line).map_err(MutationDecodeError::Json)
    }
}

fn decode_mutations_parallel(
    lines: &[&[u8]],
    source: &Arc<Vec<u8>>,
) -> Vec<Result<SessionMutation, MutationDecodeError>> {
    let workers = std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .min(PARALLEL_REPLAY_MAX_WORKERS)
        .min(lines.len());
    if workers <= 1 {
        return lines
            .iter()
            .map(|line| decode_mutation(line, source))
            .collect();
    }

    let chunk_size = lines.len().div_ceil(workers);
    std::thread::scope(|scope| {
        let handles = lines
            .chunks(chunk_size)
            .map(|chunk| {
                let source = Arc::clone(source);
                scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|line| decode_mutation(line, &source))
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>();
        let mut decoded = Vec::with_capacity(lines.len());
        for handle in handles {
            match handle.join() {
                Ok(mut chunk) => decoded.append(&mut chunk),
                Err(payload) => std::panic::resume_unwind(payload),
            }
        }
        decoded
    })
}

fn replay_capacities(lines: &[&[u8]]) -> (usize, usize) {
    let mut entries = 0usize;
    let mut records = 0usize;
    let mut unclassified = 0usize;
    for line in lines {
        if line.starts_with(br#"{"kind":"entry","#) {
            entries += 1;
        } else if line.starts_with(br#"{"kind":"record","#) {
            records += 1;
        } else if !line.starts_with(br#"{"kind":"lane","#)
            && !line.starts_with(br#"{"kind":"fact","#)
        {
            // Alternate valid whitespace or field ordering is decoded by the
            // compatibility path. Preserve the old balanced estimate for
            // those lines rather than assuming their mutation kind.
            unclassified += 1;
        }
    }
    (
        entries + unclassified.div_ceil(2),
        records + unclassified / 2,
    )
}

fn load_file(path: &Path) -> Result<(SessionHeader, SessionState, TailRepair), SessionError> {
    let bytes = Arc::new(std::fs::read(path)?);
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

    let mutation_count = lines.len().saturating_sub(1);
    let (entry_capacity, record_capacity) = replay_capacities(&lines[1..]);
    let mut state =
        SessionState::with_replay_capacities(mutation_count, entry_capacity, record_capacity);
    let mut offset = header_line.len() + usize::from(lines.len() > 1 || terminated);
    let mut decoded = (lines.len().saturating_sub(1) >= PARALLEL_REPLAY_MIN_MUTATIONS
        && bytes.len() >= PARALLEL_REPLAY_MIN_BYTES)
        .then(|| decode_mutations_parallel(&lines[1..], &bytes).into_iter());
    for (index, line) in lines.iter().enumerate().skip(1) {
        let line_number = index + 1;
        let is_last = index == lines.len() - 1;
        let result = decoded.as_mut().map_or_else(
            || decode_mutation(line, &bytes),
            |decoded| {
                decoded
                    .next()
                    .expect("parallel replay decoder preserves line count")
            },
        );
        let mutation = match result {
            Ok(mutation) => mutation,
            Err(error) if is_last && error.is_torn_json() => {
                return Ok((header, state, TailRepair::TruncateTo(offset)));
            }
            Err(error) => {
                return Err(SessionError::InvalidJson {
                    line: line_number,
                    message: error.message(),
                });
            }
        };
        validate_ambiguous_optional_fields(line, &mutation, line_number)?;
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

fn validate_ambiguous_optional_fields(
    line_bytes: &[u8],
    mutation: &SessionMutation,
    line: usize,
) -> Result<(), SessionError> {
    let field = match mutation {
        SessionMutation::Entry { lane: None, .. } => Some("lane"),
        SessionMutation::Fact {
            fact: SessionFact::Name { name: None },
            ..
        } => Some("name"),
        SessionMutation::Fact {
            fact: SessionFact::Label { label: None, .. },
            ..
        } => Some("label"),
        _ => None,
    };
    let Some(field) = field else {
        return Ok(());
    };
    let value: Value =
        serde_json::from_slice(line_bytes).map_err(|error| SessionError::InvalidJson {
            line,
            message: error.to_string(),
        })?;
    let object = value
        .as_object()
        .ok_or_else(|| invalid_json_shape(line, "is not a JSON object"))?;
    optional_json_string(object, field, line)?;
    Ok(())
}

pub(crate) fn validate_header(header: &SessionHeader) -> Result<(), SessionError> {
    header.workspace()?;
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
    fn open_handle_replays_once_and_defers_document_materialization() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let log = SessionLog::create(&path, header()).unwrap();
        let id = log
            .append_message(Message::User(UserMessage::text("hello", 1)))
            .unwrap();
        drop(log);

        let reopened = SessionLog::open_handle(&path).unwrap();

        assert_eq!(reopened.leaf_id().as_deref(), Some(id.as_str()));
        assert_eq!(reopened.load().unwrap().messages().len(), 1);
    }

    #[test]
    fn shared_document_is_stable_until_the_next_mutation() {
        let directory = tempfile::tempdir().unwrap();
        let log =
            SessionLog::create_deferred(directory.path().join("session.jsonl"), header()).unwrap();
        log.append_message(Message::User(UserMessage::text("first", 1)))
            .unwrap();

        let first = log.shared_document().unwrap();
        let same_revision = log.shared_document().unwrap();
        assert!(Arc::ptr_eq(&first, &same_revision));
        assert_eq!(first.messages().len(), 1);

        log.append_message(Message::User(UserMessage::text("second", 2)))
            .unwrap();
        let next_revision = log.shared_document().unwrap();
        assert!(!Arc::ptr_eq(&first, &next_revision));
        assert_eq!(first.messages().len(), 1);
        assert_eq!(next_revision.messages().len(), 2);
        assert!(Arc::ptr_eq(&next_revision, &log.shared_document().unwrap()));
    }

    #[test]
    fn replayed_messages_share_the_immutable_file_source() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let log = SessionLog::create(&path, header()).unwrap();
        log.append_batch([
            SessionEntry::message(Message::User(UserMessage::text("first", 1))),
            SessionEntry::message(Message::User(UserMessage::text("second", 2))),
        ])
        .unwrap();
        drop(log);

        let reopened = SessionLog::open_handle(&path).unwrap();
        let entries = reopened.branch_entries().unwrap();
        let SessionEntry::Message(first) = &entries[0].entry else {
            panic!("expected message entry");
        };
        let SessionEntry::Message(second) = &entries[1].entry else {
            panic!("expected message entry");
        };

        assert!(first.message.shares_replay_source_with(&second.message));
    }

    #[test]
    fn parallel_replay_preserves_order_shared_source_and_torn_tail_repair() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large-session.jsonl");
        let log = SessionLog::create(&path, header()).unwrap();
        let payload = "x".repeat(512);
        log.append_batch((0..2_048).map(|index| {
            SessionEntry::message(Message::User(UserMessage::text(
                format!("{index}:{payload}"),
                index,
            )))
        }))
        .unwrap();
        drop(log);
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(br#"{"kind":"entry"#)
            .unwrap();

        let reopened = SessionLog::open_handle(&path).unwrap();
        let entries = reopened.branch_entries().unwrap();

        assert_eq!(entries.len(), 2_048);
        assert!(matches!(
            &entries[0].entry,
            SessionEntry::Message(message)
                if matches!(message.message.as_standard(), Some(Message::User(user))
                    if user.content.iter().any(|block| matches!(block,
                        pi_core::ContentBlock::Text(text) if text.text.starts_with("0:"))))
        ));
        assert!(matches!(
            &entries[2_047].entry,
            SessionEntry::Message(message)
                if matches!(message.message.as_standard(), Some(Message::User(user))
                    if user.content.iter().any(|block| matches!(block,
                        pi_core::ContentBlock::Text(text) if text.text.starts_with("2047:"))))
        ));
        let (SessionEntry::Message(first), SessionEntry::Message(last)) =
            (&entries[0].entry, &entries[2_047].entry)
        else {
            panic!("expected message entries");
        };
        assert!(first.message.shares_replay_source_with(&last.message));
        assert!(std::fs::read(&path).unwrap().ends_with(b"\n"));
    }

    #[test]
    fn indexed_branch_and_context_match_document_without_materializing_it() {
        let directory = tempfile::tempdir().unwrap();
        let log =
            SessionLog::create_deferred(directory.path().join("session.jsonl"), header()).unwrap();
        let first = log
            .append_message(Message::User(UserMessage::text("first", 1)))
            .unwrap();
        let second = log
            .append_message(Message::User(UserMessage::text("second", 2)))
            .unwrap();
        log.branch(Some(&first)).unwrap();
        let sibling = log
            .append_message(Message::User(UserMessage::text("sibling", 3)))
            .unwrap();

        assert!(!log.state().has_cached_document());
        let indexed_branch = log.branch_entries().unwrap();
        let indexed_context = log.context().unwrap();
        let indexed_second_context = log
            .context_at_with_options(Some(&second), &SessionContextBuildOptions::default())
            .unwrap();
        assert!(!log.state().has_cached_document());

        let document = log.load().unwrap();
        let document_branch = document
            .branch()
            .unwrap()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            indexed_branch
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            [first.as_str(), sibling.as_str()]
        );
        assert_eq!(indexed_branch, document_branch);
        assert_eq!(indexed_context, document.context().unwrap());
        assert_eq!(
            indexed_second_context,
            document
                .context_at_with_options(Some(&second), &SessionContextBuildOptions::default())
                .unwrap()
        );
        assert!(!log.state().has_cached_document());
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
    fn read_ignores_a_torn_tail_without_repairing_the_file() {
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
        let before = std::fs::read(&path).unwrap();

        let document = SessionLog::read(&path).unwrap();

        assert_eq!(document.messages().len(), 1);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn preserves_agent_message_wire_extensions_across_jsonl_replay() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let log = SessionLog::create(&path, header()).unwrap();
        let wire = serde_json::json!({
            "role": "user",
            "content": [{
                "type": "text",
                "text": "compact input",
                "futureNested": {"keep": true}
            }],
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
        assert!(matches!(error, SessionError::InvalidJson { .. }));

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
        assert!(matches!(error, SessionError::InvalidJson { .. }));
        assert_eq!(std::fs::read_to_string(fact_path).unwrap(), original);
    }

    #[test]
    fn direct_replay_keeps_required_nullable_fields_strict() {
        let directory = tempfile::tempdir().unwrap();
        let valid_header = serde_json::to_string(&header()).unwrap();
        for (name, mutation) in [
            (
                "missing-parent",
                serde_json::json!({
                    "kind": "entry",
                    "seq": 1,
                    "id": "entry",
                    "timestamp": 1,
                    "type": "message",
                    "message": {"role": "user", "content": "hello", "timestamp": 1}
                }),
            ),
            (
                "missing-leaf",
                serde_json::json!({
                    "kind": "lane",
                    "seq": 1,
                    "lane": "review"
                }),
            ),
        ] {
            let path = directory.path().join(format!("{name}.jsonl"));
            let original = format!("{valid_header}\n{mutation}\n");
            std::fs::write(&path, &original).unwrap();

            assert!(matches!(
                SessionLog::open(&path),
                Err(SessionError::InvalidJson { .. })
            ));
            assert_eq!(std::fs::read_to_string(path).unwrap(), original);
        }
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
