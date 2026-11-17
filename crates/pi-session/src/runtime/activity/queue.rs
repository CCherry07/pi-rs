//! Durable queue editing and explicit admission of next-run input.
use super::*;

impl SessionActivity {
    pub(in crate::runtime) fn next_queued(&self) -> Option<(String, Message)> {
        self.lock()
            .recovered_queue
            .first()
            .map(|item| (item.target.id.clone(), item.message.clone()))
    }

    pub(in crate::runtime) fn begin_queued(
        &self,
        generation: u64,
        entry_id: &str,
    ) -> Result<String, SessionError> {
        let mut state = self.lock();
        if state.active_run.is_some() {
            return Err(SessionError::Busy);
        }
        let index = state
            .recovered_queue
            .iter()
            .position(|item| item.target.id == entry_id)
            .ok_or_else(|| {
                SessionError::InvalidPayload("Queued input changed before it could start".into())
            })?;
        let mut item = state.recovered_queue[index].clone();
        let run_id = next_unique_id("run");
        self.store.append_record(NewLaneRecord {
            id: run_id.clone(),
            lane: MAIN_LANE.into(),
            record: LaneRecordEntry::OperationStarted {
                source_leaf_id: self.store.leaf_id(),
                intent: OperationIntent::Run {
                    original_prompt: vec![AgentMessage::from(item.message.clone())],
                    initial_messages: vec![item.target.clone()],
                    system_prompt_override: None,
                    resume_data: None,
                },
            },
        })?;
        state.recovered_queue.remove(index);
        // The original next-run queue identity remains valid until message_end.
        // If cancelled before consumption it can be durably requeued as usual.
        item.kind = Some(QueueKind::NextRun);
        item.run_id = None;
        state.active_run = Some(ActiveSessionRun {
            id: run_id.clone(),
            generation,
            pending: vec![item],
        });
        self.phase.send_replace(ActivityPhase::Running);
        self.events.publish_queue(state.queue_snapshot());
        Ok(run_id)
    }

    /// `before` is None for cancellation, Some(None) for move-to-end, or
    /// Some(Some(id)) to move immediately before another item in the same queue.
    pub(in crate::runtime) fn edit_queue(
        &self,
        agent: &Agent,
        entry_id: &str,
        before: Option<Option<&str>>,
    ) -> Result<(), SessionError> {
        let mut state = self.lock();
        let recovered = state
            .recovered_queue
            .iter()
            .any(|item| item.target.id == entry_id);
        let kind = if recovered {
            QueueKind::NextRun
        } else {
            state
                .active_run
                .as_ref()
                .and_then(|run| {
                    run.pending
                        .iter()
                        .find(|item| item.target.id == entry_id && item.run_id.is_some())
                        .and_then(|item| item.kind)
                })
                .ok_or_else(|| {
                    SessionError::InvalidPayload(
                        "Queued input has already been consumed or changed".into(),
                    )
                })?
        };
        let pending = if recovered {
            &state.recovered_queue
        } else {
            &state
                .active_run
                .as_ref()
                .expect("live queued input")
                .pending
        };
        let original: Vec<_> = pending
            .iter()
            .filter(|item| recovered || item.kind == Some(kind))
            .cloned()
            .collect();
        let mut replacement = original.clone();
        let index = replacement
            .iter()
            .position(|item| item.target.id == entry_id)
            .expect("selected queued input");
        let item = replacement.remove(index);
        let mut records = Vec::new();
        match before {
            None => records.push(cancel_record(&item)),
            Some(before) => {
                if before == Some(entry_id) {
                    return Ok(());
                }
                let index = match before {
                    Some(id) => replacement
                        .iter()
                        .position(|item| item.target.id == id)
                        .ok_or_else(|| {
                            SessionError::InvalidPayload(
                                "Queue destination has changed or belongs to another delivery mode"
                                    .into(),
                            )
                        })?,
                    None => replacement.len(),
                };
                replacement.insert(index, item);
                if original
                    .iter()
                    .map(|item| &item.target.id)
                    .eq(replacement.iter().map(|item| &item.target.id))
                {
                    return Ok(());
                }
                // Preserve v4: one validated/synchronized batch of ordinary
                // cancellations and enqueues, with the existing prefix recovery rule.
                records.extend(original.iter().map(cancel_record));
                for item in &mut replacement {
                    item.target.id = next_unique_id("entry");
                    if recovered {
                        item.kind = Some(QueueKind::NextRun);
                        item.run_id = None;
                    }
                    records.push(NewLaneRecord {
                        id: next_unique_id("queue"),
                        lane: MAIN_LANE.into(),
                        record: LaneRecordEntry::QueueEnqueued {
                            queue: kind,
                            run_id: item.run_id.clone(),
                            target: item.target.clone(),
                        },
                    });
                }
            }
        }
        let commit = || {
            self.store
                .append_records_batch(records)
                .map(|_| ())
                .map_err(SessionError::from)
        };
        if recovered {
            commit()?;
            state.recovered_queue = replacement;
        } else {
            let destination = match kind {
                QueueKind::Steer => pi_agent::QueueDestination::Steering,
                QueueKind::FollowUp => pi_agent::QueueDestination::FollowUp,
                QueueKind::NextRun => return Err(SessionError::Busy),
            };
            let expected = original
                .iter()
                .map(|item| item.target.id.clone())
                .collect::<Vec<_>>();
            let messages = replacement
                .iter()
                .map(|item| (item.target.id.clone(), item.message.clone()))
                .collect();
            if !agent.replace_identified_queue(destination, &expected, messages, commit)? {
                return Err(SessionError::Busy);
            }
            let pending = &mut state
                .active_run
                .as_mut()
                .expect("live queued input")
                .pending;
            pending.retain(|item| item.kind != Some(kind));
            pending.extend(replacement);
        }
        self.events.publish_queue(state.queue_snapshot());
        Ok(())
    }
}

fn cancel_record(item: &PendingSessionMessage) -> NewLaneRecord {
    NewLaneRecord {
        id: next_unique_id("queue-cancel"),
        lane: MAIN_LANE.into(),
        record: LaneRecordEntry::QueueCancelled {
            run_id: item.run_id.clone(),
            entry_id: item.target.id.clone(),
        },
    }
}
