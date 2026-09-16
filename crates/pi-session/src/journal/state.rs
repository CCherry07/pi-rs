use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::{
    BranchQuery, EntryOrder, EntryQuery, ForkOptions, ForkPosition, LanePointer, LaneRecord,
    LaneRecordEntry, LogItem, MAIN_LANE, RecordQuery, SessionContext, SessionDocument,
    SessionEntry, SessionError, SessionFact, SessionHeader, SessionMutation, SessionRecord,
    SessionStats,
};

use super::validation::validate_record_query;

#[derive(Debug)]
pub(crate) struct SessionState {
    sequence: u64,
    record_ids: HashSet<String>,
    entries: Vec<SessionRecord>,
    entries_by_id: HashMap<String, usize>,
    records: Vec<LaneRecord>,
    open_operation_indices: HashMap<String, Vec<usize>>,
    lanes: Vec<LanePointer>,
    log: Vec<StoredLogItem>,
    stats: SessionStats,
    name: Option<String>,
    labels: HashMap<String, String>,
    document_cache: Option<Arc<SessionDocument>>,
}

#[derive(Debug, Clone)]
enum StoredLogItem {
    Entry {
        seq: u64,
        entry_index: usize,
    },
    Record {
        seq: u64,
        record_index: usize,
    },
    Lane {
        seq: u64,
        lane: String,
        leaf_id: Option<String>,
    },
    Fact {
        seq: u64,
        fact: SessionFact,
    },
}

impl StoredLogItem {
    fn seq(&self) -> u64 {
        match self {
            Self::Entry { seq, .. }
            | Self::Record { seq, .. }
            | Self::Lane { seq, .. }
            | Self::Fact { seq, .. } => *seq,
        }
    }

    fn materialize(&self, state: &SessionState) -> LogItem {
        match self {
            Self::Entry { seq, entry_index } => LogItem::Entry {
                seq: *seq,
                entry: state.entries[*entry_index].clone(),
            },
            Self::Record { seq, record_index } => LogItem::Record {
                seq: *seq,
                record: state.records[*record_index].clone(),
            },
            Self::Lane { seq, lane, leaf_id } => LogItem::Lane {
                seq: *seq,
                lane: lane.clone(),
                leaf_id: leaf_id.clone(),
            },
            Self::Fact { seq, fact } => LogItem::Fact {
                seq: *seq,
                fact: fact.clone(),
            },
        }
    }
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            sequence: 0,
            record_ids: HashSet::new(),
            entries: Vec::new(),
            entries_by_id: HashMap::new(),
            records: Vec::new(),
            open_operation_indices: HashMap::new(),
            lanes: vec![LanePointer {
                lane: MAIN_LANE.to_string(),
                leaf_id: None,
            }],
            log: Vec::new(),
            stats: SessionStats::default(),
            name: None,
            labels: HashMap::new(),
            document_cache: None,
        }
    }
}

impl SessionState {
    pub(crate) fn with_replay_capacities(
        mutations: usize,
        entry_capacity: usize,
        record_capacity: usize,
    ) -> Self {
        Self {
            record_ids: HashSet::with_capacity(record_capacity),
            entries: Vec::with_capacity(entry_capacity),
            entries_by_id: HashMap::with_capacity(entry_capacity),
            records: Vec::with_capacity(record_capacity),
            log: Vec::with_capacity(mutations),
            ..Self::default()
        }
    }

    pub(crate) fn next_sequence(&self) -> u64 {
        self.sequence.saturating_add(1)
    }

    pub(crate) fn require_lane(&self, lane: &str) -> Result<Option<String>, SessionError> {
        self.lanes
            .iter()
            .find(|pointer| pointer.lane == lane)
            .map(|pointer| pointer.leaf_id.clone())
            .ok_or_else(|| SessionError::InvalidLane(format!("lane not found: {lane}")))
    }

    pub(crate) fn validate_new_lane(&self, lane: &str) -> Result<(), SessionError> {
        if self.lanes.iter().any(|pointer| pointer.lane == lane) {
            return Err(SessionError::AlreadyExists(format!("lane {lane}")));
        }
        Ok(())
    }

    pub(crate) fn validate_target(&self, target: Option<&str>) -> Result<(), SessionError> {
        if let Some(id) = target
            && !self.entries_by_id.contains_key(id)
        {
            return Err(SessionError::NotFound(id.to_string()));
        }
        Ok(())
    }

    pub(crate) fn validate_unused_id(&self, id: &str) -> Result<(), SessionError> {
        if self.entries_by_id.contains_key(id) || self.record_ids.contains(id) {
            return Err(SessionError::AlreadyExists(id.to_string()));
        }
        Ok(())
    }

    pub(crate) fn validate_mutation(&self, mutation: &SessionMutation) -> Result<(), SessionError> {
        let seq = mutation.seq();
        self.validate_next_sequence(seq)?;

        match mutation {
            SessionMutation::Entry { lane, record } => {
                self.validate_entry_mutation(lane.as_deref(), record)
            }
            SessionMutation::Record { record } => self.validate_record_mutation(record),
            SessionMutation::Lane { leaf_id, .. } => self.validate_target(leaf_id.as_deref()),
            SessionMutation::Fact { fact, .. } => self.validate_fact_mutation(fact),
        }
    }

    fn validate_next_sequence(&self, seq: u64) -> Result<(), SessionError> {
        if seq != self.next_sequence() {
            return Err(SessionError::InvalidEntry(format!(
                "non-consecutive seq {seq}; expected {}",
                self.next_sequence()
            )));
        }
        Ok(())
    }

    fn validate_entry_mutation(
        &self,
        lane: Option<&str>,
        record: &SessionRecord,
    ) -> Result<(), SessionError> {
        self.validate_unused_id(&record.id)?;
        if let Some(lane) = lane {
            let leaf = self.require_lane(lane)?;
            if record.parent_id != leaf {
                return Err(SessionError::InvalidEntry(format!(
                    "entry {} does not chain to lane {lane}",
                    record.id
                )));
            }
        }
        if let Some(parent_id) = &record.parent_id
            && !self.entries_by_id.contains_key(parent_id)
        {
            return Err(SessionError::InvalidEntry(format!(
                "entry {} references missing parent {parent_id}",
                record.id
            )));
        }
        Ok(())
    }

    fn validate_record_mutation(&self, record: &LaneRecord) -> Result<(), SessionError> {
        self.require_lane(&record.lane)?;
        self.validate_unused_id(&record.id)
    }

    fn validate_fact_mutation(&self, fact: &SessionFact) -> Result<(), SessionError> {
        if let SessionFact::Label { target_id, .. } = fact {
            self.validate_target(Some(target_id))?;
        }
        Ok(())
    }

    /// Validates a mutation batch without cloning the materialized session
    /// state. Only transaction-local IDs and lane tips are staged.
    pub(crate) fn validate_mutations(
        &self,
        mutations: &[SessionMutation],
    ) -> Result<(), SessionError> {
        if let [mutation] = mutations {
            return self.validate_mutation(mutation);
        }

        let mut next_sequence = self.next_sequence();
        let mut transaction_ids = HashSet::with_capacity(mutations.len());
        let mut transaction_entry_ids = HashSet::with_capacity(mutations.len());
        let mut lane_tips = self
            .lanes
            .iter()
            .map(|pointer| (pointer.lane.clone(), pointer.leaf_id.clone()))
            .collect::<HashMap<_, _>>();

        for mutation in mutations {
            let seq = mutation.seq();
            if seq != next_sequence {
                return Err(SessionError::InvalidEntry(format!(
                    "non-consecutive seq {seq}; expected {next_sequence}"
                )));
            }

            let target_exists = |id: &str| {
                self.entries_by_id.contains_key(id) || transaction_entry_ids.contains(id)
            };
            let validate_unused_id = |id: &str| {
                if self.entries_by_id.contains_key(id)
                    || self.record_ids.contains(id)
                    || transaction_ids.contains(id)
                {
                    Err(SessionError::AlreadyExists(id.to_string()))
                } else {
                    Ok(())
                }
            };

            match mutation {
                SessionMutation::Entry { lane, record } => {
                    validate_unused_id(&record.id)?;
                    if let Some(lane) = lane {
                        let leaf = lane_tips.get(lane).ok_or_else(|| {
                            SessionError::InvalidLane(format!("lane not found: {lane}"))
                        })?;
                        if &record.parent_id != leaf {
                            return Err(SessionError::InvalidEntry(format!(
                                "entry {} does not chain to lane {lane}",
                                record.id
                            )));
                        }
                    }
                    if let Some(parent_id) = &record.parent_id
                        && !target_exists(parent_id)
                    {
                        return Err(SessionError::InvalidEntry(format!(
                            "entry {} references missing parent {parent_id}",
                            record.id
                        )));
                    }
                    transaction_ids.insert(record.id.as_str());
                    transaction_entry_ids.insert(record.id.as_str());
                    if let Some(lane) = lane {
                        lane_tips.insert(lane.clone(), Some(record.id.clone()));
                    }
                }
                SessionMutation::Record { record } => {
                    if !lane_tips.contains_key(&record.lane) {
                        return Err(SessionError::InvalidLane(format!(
                            "lane not found: {}",
                            record.lane
                        )));
                    }
                    validate_unused_id(&record.id)?;
                    transaction_ids.insert(record.id.as_str());
                }
                SessionMutation::Lane { lane, leaf_id, .. } => {
                    if let Some(leaf_id) = leaf_id
                        && !target_exists(leaf_id)
                    {
                        return Err(SessionError::NotFound(leaf_id.clone()));
                    }
                    lane_tips.insert(lane.clone(), leaf_id.clone());
                }
                SessionMutation::Fact { fact, .. } => {
                    if let SessionFact::Label { target_id, .. } = fact
                        && !target_exists(target_id)
                    {
                        return Err(SessionError::NotFound(target_id.clone()));
                    }
                }
            }
            next_sequence = next_sequence.saturating_add(1);
        }
        Ok(())
    }

    /// Applies a mutation that has already passed validation. This path is
    /// infallible so durable append can precede the in-memory state update.
    pub(crate) fn apply_validated_mutation(&mut self, mutation: SessionMutation) {
        let seq = mutation.seq();
        match mutation {
            SessionMutation::Entry { lane, record } => self.apply_entry(seq, lane, record),
            SessionMutation::Record { record } => self.apply_record(seq, record),
            SessionMutation::Lane { lane, leaf_id, .. } => self.apply_lane(seq, lane, leaf_id),
            SessionMutation::Fact { fact, .. } => self.apply_fact(seq, fact),
        }
    }

    pub(crate) fn apply_mutation(&mut self, mutation: SessionMutation) -> Result<(), SessionError> {
        let seq = mutation.seq();
        self.validate_next_sequence(seq)?;
        match mutation {
            SessionMutation::Entry { lane, record } => {
                self.validate_entry_mutation(lane.as_deref(), &record)?;
                self.apply_entry(seq, lane, record);
            }
            SessionMutation::Record { record } => {
                self.validate_record_mutation(&record)?;
                self.apply_record(seq, record);
            }
            SessionMutation::Lane { lane, leaf_id, .. } => {
                self.validate_target(leaf_id.as_deref())?;
                self.apply_lane(seq, lane, leaf_id);
            }
            SessionMutation::Fact { fact, .. } => {
                self.validate_fact_mutation(&fact)?;
                self.apply_fact(seq, fact);
            }
        }
        Ok(())
    }

    fn apply_entry(&mut self, seq: u64, lane: Option<String>, record: SessionRecord) {
        self.document_cache = None;
        self.sequence = seq;
        if matches!(record.entry, SessionEntry::Message(_)) {
            self.stats.message_count = self.stats.message_count.saturating_add(1);
        }
        let entry_index = self.entries.len();
        let entry_id = record.id.clone();
        if let Some(lane) = lane {
            self.lanes
                .iter_mut()
                .find(|pointer| pointer.lane == lane)
                .expect("validated mutation lane must remain present")
                .leaf_id = Some(entry_id.clone());
        }
        self.entries.push(record);
        self.entries_by_id.insert(entry_id, entry_index);
        self.log.push(StoredLogItem::Entry { seq, entry_index });
    }

    fn apply_record(&mut self, seq: u64, record: LaneRecord) {
        self.document_cache = None;
        self.sequence = seq;
        self.record_ids.insert(record.id.clone());
        if let LaneRecordEntry::Usage(usage) = &record.record {
            self.stats.cached_tokens = self
                .stats
                .cached_tokens
                .saturating_add(usage.usage.cache_read);
            self.stats.uncached_tokens = self
                .stats
                .uncached_tokens
                .saturating_add(usage.usage.input)
                .saturating_add(usage.usage.cache_write);
            self.stats.total_tokens = self
                .stats
                .total_tokens
                .saturating_add(usage.usage.total_tokens);
            self.stats.cost_total += usage.usage.cost.total;
        }
        let record_index = self.records.len();
        let started_lane = matches!(&record.record, LaneRecordEntry::OperationStarted { .. })
            .then(|| record.lane.clone());
        let finished_operation = match &record.record {
            LaneRecordEntry::OperationFinished { run_id, .. } => {
                Some((record.lane.clone(), run_id.clone()))
            }
            _ => None,
        };
        self.records.push(record);
        if let Some(lane) = started_lane {
            self.open_operation_indices
                .entry(lane)
                .or_default()
                .push(record_index);
        }
        if let Some((lane, run_id)) = finished_operation
            && let Some(open) = self.open_operation_indices.get_mut(&lane)
        {
            open.retain(|index| self.records[*index].id != run_id);
        }
        self.log.push(StoredLogItem::Record { seq, record_index });
    }

    fn apply_lane(&mut self, seq: u64, lane: String, leaf_id: Option<String>) {
        self.document_cache = None;
        self.sequence = seq;
        if let Some(pointer) = self.lanes.iter_mut().find(|pointer| pointer.lane == lane) {
            pointer.leaf_id.clone_from(&leaf_id);
        } else {
            self.lanes.push(LanePointer {
                lane: lane.clone(),
                leaf_id: leaf_id.clone(),
            });
        }
        self.log.push(StoredLogItem::Lane { seq, lane, leaf_id });
    }

    fn apply_fact(&mut self, seq: u64, fact: SessionFact) {
        self.document_cache = None;
        self.sequence = seq;
        match &fact {
            SessionFact::Name { name } => self.name.clone_from(name),
            SessionFact::Label { target_id, label } => {
                if let Some(label) = label {
                    self.labels.insert(target_id.clone(), label.clone());
                } else {
                    self.labels.remove(target_id);
                }
            }
        }
        self.log.push(StoredLogItem::Fact { seq, fact });
    }

    pub(crate) fn get_entry(&self, id: &str) -> Option<SessionRecord> {
        self.entry_by_id(id).cloned()
    }

    pub(crate) fn lanes(&self) -> Vec<LanePointer> {
        self.lanes.clone()
    }

    pub(crate) fn find_entries(
        &self,
        query: &EntryQuery,
    ) -> Result<Vec<SessionRecord>, SessionError> {
        validate_limit(query.limit)?;
        validate_cursor(query.after_seq)?;
        let source: Box<dyn Iterator<Item = &SessionRecord> + '_> = match query.order {
            EntryOrder::OldestFirst => Box::new(self.entries.iter()),
            EntryOrder::NewestFirst => Box::new(self.entries.iter().rev()),
        };
        let mut results = Vec::new();
        for entry in source {
            if !matches_entry_query(entry, query) {
                continue;
            }
            results.push(entry.clone());
            if results.len() == query.limit.unwrap_or(usize::MAX) {
                break;
            }
        }
        Ok(results)
    }

    pub(crate) fn find_entries_on_branch(
        &self,
        query: &BranchQuery,
        default_lane: &str,
    ) -> Result<Vec<SessionRecord>, SessionError> {
        validate_limit(query.entries.limit)?;
        validate_cursor(query.entries.after_seq)?;
        let start = match &query.start {
            Some(start) => Some(start.clone()),
            None => self.require_lane(default_lane)?,
        };
        let Some(start) = start else {
            return Ok(Vec::new());
        };
        let mut path = self.walk_to_root(&start)?;
        let mut results = Vec::new();
        match query.entries.order {
            EntryOrder::NewestFirst => {
                for entry in path {
                    let reached_bound = query.stop_at_id.as_deref() == Some(entry.id.as_str())
                        || query.stop_at_type == Some(entry.entry.entry_type());
                    if matches_entry_query(entry, &query.entries) {
                        results.push(entry.clone());
                    }
                    if reached_bound || results.len() == query.entries.limit.unwrap_or(usize::MAX) {
                        break;
                    }
                }
            }
            EntryOrder::OldestFirst => {
                path.reverse();
                for entry in path {
                    let reached_bound = query.stop_at_id.as_deref() == Some(entry.id.as_str())
                        || query.stop_at_type == Some(entry.entry.entry_type());
                    if matches_entry_query(entry, &query.entries) {
                        results.push(entry.clone());
                    }
                    if reached_bound || results.len() == query.entries.limit.unwrap_or(usize::MAX) {
                        break;
                    }
                }
            }
        }
        Ok(results)
    }

    pub(crate) fn branch_entries_for_lane(
        &self,
        lane: &str,
    ) -> Result<Vec<SessionRecord>, SessionError> {
        self.branch_entries_at(self.require_lane(lane)?.as_deref())
    }

    pub(crate) fn branch_entries_at(
        &self,
        leaf_id: Option<&str>,
    ) -> Result<Vec<SessionRecord>, SessionError> {
        let mut path = match leaf_id {
            Some(id) => self.walk_to_root(id)?,
            None => Vec::new(),
        };
        path.reverse();
        Ok(path.into_iter().cloned().collect())
    }

    pub(crate) fn default_context_for_lane(
        &self,
        lane: &str,
    ) -> Result<SessionContext, SessionError> {
        self.default_context_at(self.require_lane(lane)?.as_deref())
    }

    pub(crate) fn default_context_at(
        &self,
        leaf_id: Option<&str>,
    ) -> Result<SessionContext, SessionError> {
        let mut path = match leaf_id {
            Some(id) => self.walk_to_root(id)?,
            None => Vec::new(),
        };
        path.reverse();
        Ok(crate::context::build_default_session_context_from_refs(
            &path,
        ))
    }

    pub(crate) fn find_records(
        &self,
        query: &RecordQuery,
    ) -> Result<Vec<LaneRecord>, SessionError> {
        validate_record_query(query)?;
        validate_limit(query.limit)?;
        validate_cursor(query.after_seq)?;
        let source: Box<dyn Iterator<Item = &LaneRecord> + '_> = match query.order {
            EntryOrder::OldestFirst => Box::new(self.records.iter()),
            EntryOrder::NewestFirst => Box::new(self.records.iter().rev()),
        };
        let mut results = Vec::new();
        for record in source {
            if !matches_record_query(record, query) {
                continue;
            }
            results.push(record.clone());
            if results.len() == query.limit.unwrap_or(usize::MAX) {
                break;
            }
        }
        Ok(results)
    }

    pub(crate) fn find_open_operations(
        &self,
        lane: &str,
        limit: Option<usize>,
    ) -> Result<Vec<LaneRecord>, SessionError> {
        validate_limit(limit)?;
        Ok(self
            .open_operation_indices
            .get(lane)
            .into_iter()
            .flatten()
            .rev()
            .take(limit.unwrap_or(usize::MAX))
            .map(|index| self.records[*index].clone())
            .collect())
    }

    pub(crate) fn get_log(
        &self,
        after_seq: Option<u64>,
        limit: Option<usize>,
    ) -> Result<Vec<LogItem>, SessionError> {
        validate_limit(limit)?;
        validate_cursor(after_seq)?;
        Ok(self
            .log
            .iter()
            .filter(|item| after_seq.is_none_or(|after| item.seq() > after))
            .take(limit.unwrap_or(usize::MAX))
            .map(|item| item.materialize(self))
            .collect())
    }

    pub(crate) fn name(&self) -> Option<String> {
        self.name.clone()
    }

    pub(crate) fn label(&self, id: &str) -> Option<String> {
        self.labels.get(id).cloned()
    }

    pub(crate) fn stats(&self) -> SessionStats {
        self.stats.clone()
    }

    pub(crate) fn document(&self, header: SessionHeader) -> SessionDocument {
        SessionDocument {
            header,
            entries: self.entries.clone(),
            records: self.records.clone(),
            lanes: self.lanes.clone(),
            log: self.log.iter().map(|item| item.materialize(self)).collect(),
            name: self.name.clone(),
            labels: self.labels.clone(),
            stats: self.stats.clone(),
        }
    }

    /// Returns the immutable document for the current mutation revision.
    /// Repeated readers share the same materialization until the next
    /// successfully applied mutation invalidates it.
    pub(crate) fn shared_document(&mut self, header: SessionHeader) -> Arc<SessionDocument> {
        if let Some(document) = &self.document_cache {
            return Arc::clone(document);
        }
        let document = Arc::new(self.document(header));
        self.document_cache = Some(Arc::clone(&document));
        document
    }

    #[cfg(test)]
    pub(crate) fn has_cached_document(&self) -> bool {
        self.document_cache.is_some()
    }

    pub(crate) fn create_fork_mutations(
        &self,
        options: &ForkOptions,
    ) -> Result<Vec<SessionMutation>, SessionError> {
        let (entries, lanes) = match options {
            ForkOptions::Tree => (self.entries.clone(), self.lanes.clone()),
            ForkOptions::Branch { entry_id, position } => {
                let selected = match entry_id {
                    Some(id) => Some(id.clone()),
                    None => self.require_lane(MAIN_LANE)?,
                };
                let target = match selected {
                    None => None,
                    Some(id) => {
                        let entry = self.entry_by_id(&id).ok_or_else(|| {
                            SessionError::InvalidForkTarget(format!("entry not found: {id}"))
                        })?;
                        if !matches!(entry.entry, SessionEntry::Message(_)) {
                            return Err(SessionError::InvalidForkTarget(format!(
                                "entry is not a message: {id}"
                            )));
                        }
                        let effective_position = position.unwrap_or(if entry_id.is_some() {
                            ForkPosition::Before
                        } else {
                            ForkPosition::At
                        });
                        match effective_position {
                            ForkPosition::At => Some(entry.id.clone()),
                            ForkPosition::Before => entry.parent_id.clone(),
                        }
                    }
                };
                let entries = match &target {
                    Some(id) => {
                        let mut path = self.walk_to_root(id)?;
                        path.reverse();
                        path.into_iter().cloned().collect()
                    }
                    None => Vec::new(),
                };
                (
                    entries,
                    vec![LanePointer {
                        lane: MAIN_LANE.to_string(),
                        leaf_id: target,
                    }],
                )
            }
        };

        Ok(self.create_snapshot_mutations(entries, lanes))
    }

    /// Projects the complete active main-lane branch as standalone mutations.
    ///
    /// Unlike an interactive fork target, the active leaf may be any session
    /// entry (for example custom metadata, model change, or compaction).
    pub(crate) fn create_main_branch_snapshot_mutations(
        &self,
    ) -> Result<Vec<SessionMutation>, SessionError> {
        let target = self.require_lane(MAIN_LANE)?;
        let entries = match &target {
            Some(id) => {
                let mut path = self.walk_to_root(id)?;
                path.reverse();
                path.into_iter().cloned().collect()
            }
            None => Vec::new(),
        };
        Ok(self.create_snapshot_mutations(
            entries,
            vec![LanePointer {
                lane: MAIN_LANE.to_string(),
                leaf_id: target,
            }],
        ))
    }

    fn create_snapshot_mutations(
        &self,
        entries: Vec<SessionRecord>,
        lanes: Vec<LanePointer>,
    ) -> Vec<SessionMutation> {
        let mut mutations = Vec::new();
        let mut seq = 1u64;
        let labels = entries
            .iter()
            .filter_map(|entry| {
                self.labels
                    .get(&entry.id)
                    .map(|label| (entry.id.clone(), label.clone()))
            })
            .collect::<Vec<_>>();
        for mut entry in entries {
            entry.seq = seq;
            mutations.push(SessionMutation::Entry {
                lane: None,
                record: entry,
            });
            seq = seq.saturating_add(1);
        }
        for pointer in lanes {
            mutations.push(SessionMutation::Lane {
                seq,
                lane: pointer.lane,
                leaf_id: pointer.leaf_id,
            });
            seq = seq.saturating_add(1);
        }
        if self.name.is_some() {
            mutations.push(SessionMutation::Fact {
                seq,
                fact: SessionFact::Name {
                    name: self.name.clone(),
                },
            });
            seq = seq.saturating_add(1);
        }
        for (target_id, label) in labels {
            mutations.push(SessionMutation::Fact {
                seq,
                fact: SessionFact::Label {
                    target_id,
                    label: Some(label),
                },
            });
            seq = seq.saturating_add(1);
        }
        mutations
    }

    fn entry_by_id(&self, id: &str) -> Option<&SessionRecord> {
        self.entries_by_id
            .get(id)
            .and_then(|index| self.entries.get(*index))
    }

    fn walk_to_root<'a>(&'a self, start: &str) -> Result<Vec<&'a SessionRecord>, SessionError> {
        let mut path = Vec::new();
        let mut visited = HashSet::new();
        let mut current = self
            .entry_by_id(start)
            .ok_or_else(|| SessionError::NotFound(start.to_string()))?;
        loop {
            if !visited.insert(current.id.as_str()) {
                return Err(SessionError::InvalidEntry(format!(
                    "session branch contains a cycle at {}",
                    current.id
                )));
            }
            path.push(current);
            let Some(parent_id) = &current.parent_id else {
                break;
            };
            current = self.entry_by_id(parent_id).ok_or_else(|| {
                SessionError::InvalidEntry(format!("entry not found: {parent_id}"))
            })?;
        }
        Ok(path)
    }
}

fn validate_limit(limit: Option<usize>) -> Result<(), SessionError> {
    if limit == Some(0) {
        return Err(SessionError::InvalidQuery(
            "limit must be a positive integer".to_string(),
        ));
    }
    Ok(())
}

fn validate_cursor(after_seq: Option<u64>) -> Result<(), SessionError> {
    // Rust's unsigned cursor type already excludes the invalid values checked
    // by Pi's JavaScript facade (negative and fractional numbers).
    let _ = after_seq;
    Ok(())
}

fn matches_entry_query(entry: &SessionRecord, query: &EntryQuery) -> bool {
    let type_matches = query
        .entry_type
        .is_none_or(|entry_type| entry.entry.entry_type() == entry_type);
    let custom_matches = query.custom_type.as_ref().is_none_or(|custom_type| {
        matches!(&entry.entry, SessionEntry::Custom(custom) if custom.custom_type == *custom_type)
    });
    let cursor_matches = query.after_seq.is_none_or(|after| match query.order {
        EntryOrder::OldestFirst => entry.seq > after,
        EntryOrder::NewestFirst => entry.seq < after,
    });
    type_matches && custom_matches && cursor_matches
}

fn matches_record_query(record: &LaneRecord, query: &RecordQuery) -> bool {
    let operation_matches = query.operation_kind.is_none_or(|kind| {
        matches!(
            &record.record,
            LaneRecordEntry::OperationStarted { intent, .. } if intent.kind() == kind
        )
    });
    let run_matches = query.run_id.as_ref().is_none_or(|run_id| {
        if matches!(record.record, LaneRecordEntry::OperationStarted { .. }) {
            record.id == *run_id
        } else {
            record.record.run_id() == Some(run_id.as_str())
        }
    });
    query.lane.as_ref().is_none_or(|lane| record.lane == *lane)
        && query
            .record_type
            .is_none_or(|record_type| record.record.record_type() == record_type)
        && run_matches
        && operation_matches
        && query.after_seq.is_none_or(|after| record.seq > after)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::CustomEntry;

    fn payload_pointer(record: &SessionRecord) -> usize {
        let SessionEntry::Custom(custom) = &record.entry else {
            panic!("expected custom entry");
        };
        custom
            .data
            .as_ref()
            .and_then(|data| data.get("payload"))
            .and_then(serde_json::Value::as_str)
            .expect("payload text")
            .as_ptr() as usize
    }

    #[test]
    fn session_state_retains_one_entry_payload_copy() {
        let mut state = SessionState::default();
        state
            .apply_mutation(SessionMutation::Entry {
                lane: Some(MAIN_LANE.to_string()),
                record: SessionRecord {
                    id: "entry".to_string(),
                    seq: 1,
                    parent_id: None,
                    timestamp_ms: 1,
                    entry: SessionEntry::Custom(CustomEntry {
                        custom_type: "benchmark".to_string(),
                        data: Some(json!({ "payload": "x".repeat(4_096) })),
                    }),
                },
            })
            .unwrap();

        let entry_index = *state.entries_by_id.get("entry").unwrap();
        let log_entry_index = match &state.log[0] {
            StoredLogItem::Entry { entry_index, .. } => *entry_index,
            _ => panic!("expected entry log item"),
        };
        let pointers = [
            payload_pointer(&state.entries[0]),
            payload_pointer(&state.entries[entry_index]),
            payload_pointer(&state.entries[log_entry_index]),
        ]
        .into_iter()
        .collect::<HashSet<_>>();

        assert_eq!(
            pointers.len(),
            1,
            "the canonical entry payload should be retained only once"
        );
    }

    #[test]
    fn atomic_validation_tracks_transaction_local_entries_and_lane_tips() {
        let mut state = SessionState::default();
        let mutations = vec![
            SessionMutation::Entry {
                lane: Some(MAIN_LANE.to_string()),
                record: SessionRecord {
                    id: "root".to_string(),
                    seq: 1,
                    parent_id: None,
                    timestamp_ms: 1,
                    entry: SessionEntry::Custom(CustomEntry {
                        custom_type: "root".to_string(),
                        data: None,
                    }),
                },
            },
            SessionMutation::Lane {
                seq: 2,
                lane: "worker".to_string(),
                leaf_id: Some("root".to_string()),
            },
            SessionMutation::Entry {
                lane: Some("worker".to_string()),
                record: SessionRecord {
                    id: "child".to_string(),
                    seq: 3,
                    parent_id: Some("root".to_string()),
                    timestamp_ms: 2,
                    entry: SessionEntry::Custom(CustomEntry {
                        custom_type: "child".to_string(),
                        data: None,
                    }),
                },
            },
            SessionMutation::Fact {
                seq: 4,
                fact: SessionFact::Label {
                    target_id: "child".to_string(),
                    label: Some("checkpoint".to_string()),
                },
            },
        ];

        state.validate_mutations(&mutations).unwrap();
        for mutation in mutations {
            state.apply_validated_mutation(mutation);
        }

        assert_eq!(state.sequence, 4);
        assert_eq!(
            state.require_lane("worker").unwrap().as_deref(),
            Some("child")
        );
        assert_eq!(state.label("child").as_deref(), Some("checkpoint"));

        let invalid = vec![SessionMutation::Entry {
            lane: Some(MAIN_LANE.to_string()),
            record: SessionRecord {
                id: "root".to_string(),
                seq: 5,
                parent_id: Some("root".to_string()),
                timestamp_ms: 3,
                entry: SessionEntry::Custom(CustomEntry {
                    custom_type: "duplicate".to_string(),
                    data: None,
                }),
            },
        }];
        assert!(matches!(
            state.validate_mutations(&invalid),
            Err(SessionError::AlreadyExists(_))
        ));
        assert_eq!(state.sequence, 4, "validation must not mutate live state");
    }
}
