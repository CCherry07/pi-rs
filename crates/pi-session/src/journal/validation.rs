use pi_core::{Message, Usage, UsageCost};

use crate::{
    AgentMessage, LaneRecordEntry, NewLaneRecord, OperationIntent, ProvisionedEntry, RecordQuery,
    SessionEntry, SessionError, SessionMutation,
};

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
        SessionEntry::Custom(_) if crate::isolated_context::is_seed(entry) => {
            let seed = crate::isolated_context::read_seed(entry).ok_or_else(|| {
                SessionError::InvalidPayload("invalid isolated context seed".to_string())
            })?;
            for message in &seed.messages {
                validate_agent_message(message)?;
            }
            Ok(())
        }
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
        | SessionEntry::CustomMessage(_)
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
        Some(Message::User(_) | Message::Custom(_)) | None => Ok(()),
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

pub(crate) fn validate_record_query(query: &RecordQuery) -> Result<(), SessionError> {
    if query.operation_kind.is_some()
        && query.record_type != Some(crate::LaneRecordType::OperationStarted)
    {
        return Err(SessionError::InvalidQuery(
            "operation_kind requires operation_started record type".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use pi_core::{
        AssistantMessage, Message, ModelId, ProviderId, StopReason, Usage, UsageCost, UserMessage,
    };

    use super::*;
    use crate::{
        BranchQuery, EntryOrder, EntryQuery, LaneRecordEntry, LogItem, MAIN_LANE, OperationIntent,
        OperationOutcome, ProvisionedEntry, RecordQuery, SessionEntry, SessionEntryType,
        SessionHeader, SessionLog, SessionStats, SessionUsage, UsageAttribution, UsageRecord,
    };

    fn deferred_session() -> SessionLog {
        let directory = tempfile::tempdir().unwrap();
        SessionLog::create_deferred(
            directory.path().join("session.jsonl"),
            SessionHeader::new("session", directory.path()),
        )
        .unwrap()
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
    fn all_mutation_kinds_share_one_sequence_and_lanes_share_one_tree() {
        let session = deferred_session();
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
        assert_eq!(session.lanes()[0].leaf_id.as_deref(), Some("child"));
        assert_eq!(session.lanes()[1].leaf_id.as_deref(), Some("child"));
    }

    #[test]
    fn record_queries_and_open_operation_invariant_match_pi() {
        let session = deferred_session();
        assert!(
            session
                .find_open_operations("missing", Some(2))
                .unwrap()
                .is_empty()
        );
        let invalid = session
            .find_records(&RecordQuery {
                operation_kind: Some(crate::OperationKind::Run),
                ..RecordQuery::default()
            })
            .unwrap_err();
        assert!(matches!(invalid, SessionError::InvalidQuery(_)));
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
        let session = deferred_session();
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
        assert!(matches!(error, SessionError::InvalidPayload(_)));
        assert!(session.get_log(None, None).unwrap().is_empty());
    }

    #[test]
    fn filtered_branch_queries_and_signed_usage_ledger_match_pi() {
        let session = deferred_session();
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
                .find_entries(&EntryQuery {
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
                .find_entries_on_branch(&BranchQuery {
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
            session.stats(),
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
