use super::activity::RecoveredQueue;
use crate::{
    EntryOrder, LaneRecordEntry, MAIN_LANE, NewLaneRecord, OperationError, OperationOutcome,
    ProvisionedEntry, RecordQuery, SessionEntry, SessionError, SessionStore, next_unique_id,
};

/// Read-only recovery preparation shared by generation configuration and journal repair.
/// Only interrupted sessions require the reducer's complete document view.
#[derive(Default)]
pub(crate) struct SessionRecoveryPlan {
    pending_writes: Vec<ProvisionedEntry>,
    missing_initial_messages: Vec<ProvisionedEntry>,
}

impl SessionRecoveryPlan {
    pub(crate) fn prepare(store: &SessionStore) -> Result<Self, SessionError> {
        let open_operations = store.find_open_operations(MAIN_LANE, None)?;
        let Some(started) = open_operations.first() else {
            return Ok(Self::default());
        };
        // Reducer recovery is intentionally the slow path. It needs the full
        // tree and record log to reconstruct accepted writes from an
        // interrupted operation, while settled sessions never materialize a
        // SessionDocument during startup.
        let document = store.shared_document()?;
        let branch = document.branch()?;
        let (state, _) = crate::history::reduce_lane_operation(
            &crate::RecordLogSlice {
                lane: MAIN_LANE.to_string(),
                open_operations: open_operations.clone(),
                records: document
                    .records
                    .iter()
                    .filter(|record| record.lane == MAIN_LANE)
                    .cloned()
                    .collect(),
                entries: document.entries.clone(),
            },
            document
                .lanes
                .iter()
                .find(|lane| lane.lane == MAIN_LANE)
                .and_then(|lane| lane.leaf_id.as_deref()),
            &branch
                .iter()
                .filter(|entry| entry.seq > started.seq)
                .map(|entry| (*entry).clone())
                .collect::<Vec<_>>(),
        )
        .map_err(|error| {
            SessionError::InvalidPayload(format!("invalid interrupted operation log: {error}"))
        })?;
        Ok(state
            .operation
            .map_or_else(Self::default, |operation| Self {
                pending_writes: operation.pending_writes,
                missing_initial_messages: operation.missing_initial_messages,
            }))
    }

    pub(crate) fn configuration(
        &self,
        store: &SessionStore,
    ) -> Result<crate::RestoredSessionConfiguration, SessionError> {
        let selected = store.find_entries_on_branch(&crate::BranchQuery {
            entries: crate::EntryQuery {
                entry_type: Some(crate::SessionEntryType::ActiveToolsChange),
                limit: Some(1),
                ..crate::EntryQuery::default()
            },
            ..crate::BranchQuery::default()
        })?;
        let mut active_tools = selected.into_iter().find_map(|record| match record.entry {
            SessionEntry::ActiveToolsChange(change) => Some(change.active_tool_names),
            _ => None,
        });
        // The repair below appends accepted writes in this exact reducer-provided order.
        for target in &self.pending_writes {
            if let SessionEntry::ActiveToolsChange(change) = &target.entry {
                active_tools = Some(change.active_tool_names.clone());
            }
        }
        Ok(crate::RestoredSessionConfiguration {
            active_tools: active_tools.map_or(
                crate::RestoredToolSelection::RuntimeDefault,
                crate::RestoredToolSelection::Selected,
            ),
        })
    }
}

pub(super) fn recover_interrupted_state(
    store: &SessionStore,
    plan: SessionRecoveryPlan,
) -> Result<RecoveredQueue, SessionError> {
    let SessionRecoveryPlan {
        pending_writes,
        missing_initial_messages,
    } = plan;
    // Accepted writes are idempotent: the reducer only returns targets with absent IDs.
    for target in pending_writes {
        store.append_entry(target, MAIN_LANE)?;
    }

    // Queue recovery and operation closure only need the append-only record
    // ledger. Query it after reducer repairs so this view is current without
    // cloning the entry tree, facts, lane pointers, and interleaved log.
    let records = store.find_records(&RecordQuery {
        order: EntryOrder::OldestFirst,
        ..RecordQuery::default()
    })?;
    // Preserve provisioned and undelivered input as durable next-run items.
    // Recovery never invokes providers or replays tool execution.
    let recovered = RecoveredQueue::recover(store, &records, missing_initial_messages)?;

    let finished = records
        .iter()
        .filter_map(|record| match &record.record {
            LaneRecordEntry::OperationFinished { run_id, .. } => Some(run_id.as_str()),
            _ => None,
        })
        .collect::<std::collections::HashSet<_>>();
    for operation in records.iter().filter(|record| {
        matches!(record.record, LaneRecordEntry::OperationStarted { .. })
            && !finished.contains(record.id.as_str())
    }) {
        store.append_record(NewLaneRecord {
            id: next_unique_id("recovery"),
            lane: operation.lane.clone(),
            record: LaneRecordEntry::OperationFinished {
                run_id: operation.id.clone(),
                outcome: OperationOutcome::Aborted,
                error: Some(OperationError {
                    code: "interrupted".to_string(),
                    message: "operation was interrupted by process termination".to_string(),
                }),
            },
        })?;
    }
    Ok(recovered)
}
