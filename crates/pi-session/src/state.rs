use std::collections::{HashMap, HashSet};

use crate::{
    BranchQuery, EntryOrder, EntryQuery, ForkOptions, ForkPosition, LanePointer, LaneRecord,
    LaneRecordEntry, LogItem, MAIN_LANE, RecordQuery, SessionDocument, SessionEntry, SessionError,
    SessionFact, SessionHeader, SessionMutation, SessionRecord, SessionStats,
};

#[derive(Debug, Clone)]
pub(crate) struct SessionState {
    sequence: u64,
    used_ids: HashSet<String>,
    entries: Vec<SessionRecord>,
    entries_by_id: HashMap<String, SessionRecord>,
    records: Vec<LaneRecord>,
    lanes: Vec<LanePointer>,
    log: Vec<LogItem>,
    stats: SessionStats,
    name: Option<String>,
    labels: HashMap<String, String>,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            sequence: 0,
            used_ids: HashSet::new(),
            entries: Vec::new(),
            entries_by_id: HashMap::new(),
            records: Vec::new(),
            lanes: vec![LanePointer {
                lane: MAIN_LANE.to_string(),
                leaf_id: None,
            }],
            log: Vec::new(),
            stats: SessionStats::default(),
            name: None,
            labels: HashMap::new(),
        }
    }
}

impl SessionState {
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
        if self.used_ids.contains(id) {
            return Err(SessionError::AlreadyExists(id.to_string()));
        }
        Ok(())
    }

    pub(crate) fn apply_mutation(&mut self, mutation: SessionMutation) -> Result<(), SessionError> {
        let seq = mutation.seq();
        if seq != self.next_sequence() {
            return Err(SessionError::InvalidEntry(format!(
                "non-consecutive seq {seq}; expected {}",
                self.next_sequence()
            )));
        }

        match mutation {
            SessionMutation::Entry { lane, record } => {
                self.validate_unused_id(&record.id)?;
                if let Some(lane) = &lane {
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

                self.sequence = seq;
                self.used_ids.insert(record.id.clone());
                if matches!(record.entry, SessionEntry::Message(_)) {
                    self.stats.message_count = self.stats.message_count.saturating_add(1);
                }
                self.entries.push(record.clone());
                self.entries_by_id.insert(record.id.clone(), record.clone());
                if let Some(lane) = lane {
                    self.lane_mut(&lane)?.leaf_id = Some(record.id.clone());
                }
                self.log.push(LogItem::Entry { seq, entry: record });
            }
            SessionMutation::Record { record } => {
                self.require_lane(&record.lane)?;
                self.validate_unused_id(&record.id)?;
                self.sequence = seq;
                self.used_ids.insert(record.id.clone());
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
                self.records.push(record.clone());
                self.log.push(LogItem::Record { seq, record });
            }
            SessionMutation::Lane { lane, leaf_id, .. } => {
                self.validate_target(leaf_id.as_deref())?;
                self.sequence = seq;
                if let Some(pointer) = self.lanes.iter_mut().find(|pointer| pointer.lane == lane) {
                    pointer.leaf_id.clone_from(&leaf_id);
                } else {
                    self.lanes.push(LanePointer {
                        lane: lane.clone(),
                        leaf_id: leaf_id.clone(),
                    });
                }
                self.log.push(LogItem::Lane { seq, lane, leaf_id });
            }
            SessionMutation::Fact { fact, .. } => {
                if let SessionFact::Label { target_id, .. } = &fact {
                    self.validate_target(Some(target_id))?;
                }
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
                self.log.push(LogItem::Fact { seq, fact });
            }
        }
        Ok(())
    }

    pub(crate) fn get_entry(&self, id: &str) -> Option<SessionRecord> {
        self.entries_by_id.get(id).cloned()
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
                    if matches_entry_query(&entry, &query.entries) {
                        results.push(entry);
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
                    if matches_entry_query(&entry, &query.entries) {
                        results.push(entry);
                    }
                    if reached_bound || results.len() == query.entries.limit.unwrap_or(usize::MAX) {
                        break;
                    }
                }
            }
        }
        Ok(results)
    }

    pub(crate) fn find_records(
        &self,
        query: &RecordQuery,
    ) -> Result<Vec<LaneRecord>, SessionError> {
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
        let mut open = Vec::<LaneRecord>::new();
        for record in &self.records {
            if record.lane != lane {
                continue;
            }
            match &record.record {
                LaneRecordEntry::OperationStarted { .. } => open.push(record.clone()),
                LaneRecordEntry::OperationFinished { run_id, .. } => {
                    open.retain(|started| started.id != *run_id);
                }
                _ => {}
            }
        }
        open.reverse();
        open.truncate(limit.unwrap_or(usize::MAX));
        Ok(open)
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
            .cloned()
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
            log: self.log.clone(),
            name: self.name.clone(),
            labels: self.labels.clone(),
            stats: self.stats.clone(),
        }
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
                        let entry = self.entries_by_id.get(&id).ok_or_else(|| {
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
                        path
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
    /// entry (for example a prompt snapshot, model change, or compaction).
    pub(crate) fn create_main_branch_snapshot_mutations(
        &self,
    ) -> Result<Vec<SessionMutation>, SessionError> {
        let target = self.require_lane(MAIN_LANE)?;
        let entries = match &target {
            Some(id) => {
                let mut path = self.walk_to_root(id)?;
                path.reverse();
                path
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
        for mut entry in entries.clone() {
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
        for entry in entries {
            if let Some(label) = self.labels.get(&entry.id) {
                mutations.push(SessionMutation::Fact {
                    seq,
                    fact: SessionFact::Label {
                        target_id: entry.id,
                        label: Some(label.clone()),
                    },
                });
                seq = seq.saturating_add(1);
            }
        }
        mutations
    }

    fn lane_mut(&mut self, lane: &str) -> Result<&mut LanePointer, SessionError> {
        self.lanes
            .iter_mut()
            .find(|pointer| pointer.lane == lane)
            .ok_or_else(|| SessionError::InvalidLane(format!("lane not found: {lane}")))
    }

    fn walk_to_root(&self, start: &str) -> Result<Vec<SessionRecord>, SessionError> {
        let mut path = Vec::new();
        let mut visited = HashSet::new();
        let mut current = self
            .entries_by_id
            .get(start)
            .ok_or_else(|| SessionError::NotFound(start.to_string()))?;
        loop {
            if !visited.insert(current.id.clone()) {
                return Err(SessionError::InvalidEntry(format!(
                    "session branch contains a cycle at {}",
                    current.id
                )));
            }
            path.push(current.clone());
            let Some(parent_id) = &current.parent_id else {
                break;
            };
            current = self.entries_by_id.get(parent_id).ok_or_else(|| {
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
