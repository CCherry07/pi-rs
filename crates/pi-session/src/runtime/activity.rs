use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use pi_agent::{Agent, AgentLoopOutcome, AgentLoopStop};
use pi_core::{AbortHandle, AgentEvent, ContentBlock, ImageContent, Message, UserMessage};

use super::PromptOutcome;
use crate::runtime::events::AgentSessionEventHub;
use crate::{
    AgentMessage, LaneRecord, LaneRecordEntry, MAIN_LANE, NewLaneRecord, OperationError,
    OperationIntent, OperationOutcome, SessionEntry, SessionRecord, SessionStore, next_unique_id,
    now_ms,
};
use pi_runtime::QueuedTextOutcome;
use tokio::sync::watch;

mod queue;

use crate::{ProvisionedEntry, QueueKind, QueueSnapshot, SessionError};

#[derive(Debug, Clone)]
struct PendingSessionMessage {
    kind: Option<QueueKind>,
    run_id: Option<String>,
    display_text: String,
    message: Message,
    target: ProvisionedEntry,
    /// Live steer/follow-up submissions may attach to the next explicit prompt
    /// when parked. Retiming preserves this intent; replay and explicit NextRun
    /// items keep their separate admission policy.
    deliver_on_prompt: bool,
    /// Delivery has started, so hide it from the editable queue. Keep its
    /// provisioned identity pending until the final hook-patched message commits.
    started: bool,
}

#[derive(Debug)]
struct ActiveSessionRun {
    id: String,
    generation: u64,
    pending: Vec<PendingSessionMessage>,
}

#[derive(Debug, Default)]
struct ActivityState {
    active_run: Option<ActiveSessionRun>,
    recovered_queue: Vec<PendingSessionMessage>,
}

impl ActivityState {
    fn queue_snapshot(&self) -> QueueSnapshot {
        let items = self
            .recovered_queue
            .iter()
            .chain(self.active_run.iter().flat_map(|run| run.pending.iter()));
        queue_snapshot(items)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ActivityPhase {
    Idle,
    Preparing,
    Running,
    RetryWait,
    Compacting,
    Summarizing,
    BeforeSettle,
    Settling,
    Shell,
    Updating,
}

#[derive(Clone, Copy)]
pub(super) enum CancellationStage {
    Retry,
    Compaction,
    Boundary,
}

#[derive(Default)]
struct Cancellation {
    retry: Option<AbortHandle>,
    compaction: Option<AbortHandle>,
    boundary: Option<AbortHandle>,
    shell: Option<(String, AbortHandle)>,
}

impl Cancellation {
    fn slot(&mut self, stage: CancellationStage) -> &mut Option<AbortHandle> {
        match stage {
            CancellationStage::Retry => &mut self.retry,
            CancellationStage::Compaction => &mut self.compaction,
            CancellationStage::Boundary => &mut self.boundary,
        }
    }
}

/// Owns operation admission and the complete product activity, including work
/// performed after the low-level Agent has finished. Durable delivery remains a
/// separate fact: a settling activity is busy but no longer accepts run input.
pub(super) struct SessionActivity {
    store: SessionStore,
    events: Arc<AgentSessionEventHub>,
    gate: tokio::sync::Mutex<()>,
    phase: watch::Sender<ActivityPhase>,
    state: Mutex<ActivityState>,
    cancellation: Mutex<Cancellation>,
    aborted: AtomicBool,
    stopped: AtomicBool,
    continuation: AtomicBool,
    retry_attempt: AtomicU32,
}

impl SessionActivity {
    pub(super) fn new(
        store: SessionStore,
        events: Arc<AgentSessionEventHub>,
        recovered_queue: RecoveredQueue,
    ) -> Self {
        let (phase, _) = watch::channel(ActivityPhase::Idle);
        Self {
            store,
            events,
            gate: tokio::sync::Mutex::new(()),
            phase,
            state: Mutex::new(ActivityState {
                active_run: None,
                recovered_queue: recovered_queue.0,
            }),
            cancellation: Mutex::new(Cancellation::default()),
            aborted: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            continuation: AtomicBool::new(false),
            retry_attempt: AtomicU32::new(0),
        }
    }

    fn lock(&self) -> MutexGuard<'_, ActivityState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(super) fn run_id(&self) -> Option<String> {
        self.lock().active_run.as_ref().map(|run| run.id.clone())
    }

    pub(super) fn delivery_target(&self) -> Result<LiveDelivery, SessionError> {
        let state = self.lock();
        let run = state.active_run.as_ref().ok_or(SessionError::Busy)?;
        Ok(LiveDelivery {
            run_id: run.id.clone(),
            generation: run.generation,
        })
    }

    pub(super) fn begin_text(
        &self,
        generation: u64,
        display: &str,
        text: &str,
        user_message: UserMessage,
    ) -> Result<String, SessionError> {
        let message = Message::User(user_message);
        let wire = if display == text {
            AgentMessage::from(message.clone())
        } else {
            AgentMessage::with_display_text(message.clone(), display)?
        };
        self.begin_run(generation, vec![(message, wire, display.to_owned())])
    }

    pub(super) fn begin_messages(
        &self,
        generation: u64,
        messages: &[Message],
    ) -> Result<String, SessionError> {
        if messages.is_empty() {
            return Err(SessionError::InvalidPayload(
                "structured session prompt must contain at least one message".into(),
            ));
        }
        self.begin_run(
            generation,
            messages
                .iter()
                .map(|message| {
                    (
                        message.clone(),
                        AgentMessage::from(message.clone()),
                        message_display_text(message),
                    )
                })
                .collect(),
        )
    }

    fn begin_run(
        &self,
        generation: u64,
        input: Vec<(Message, AgentMessage, String)>,
    ) -> Result<String, SessionError> {
        let mut state = self.lock();
        if state.active_run.is_some() {
            return Err(SessionError::Busy);
        }
        let run_id = next_unique_id("run");
        let original_prompt = input.iter().map(|(_, wire, _)| wire.clone()).collect();
        let pending = input
            .into_iter()
            .map(|(message, wire, display_text)| PendingSessionMessage {
                kind: None,
                run_id: Some(run_id.clone()),
                display_text,
                message,
                target: ProvisionedEntry {
                    id: next_unique_id("entry"),
                    entry: SessionEntry::message(wire),
                },
                deliver_on_prompt: false,
                started: false,
            })
            .collect::<Vec<_>>();
        self.store.append_record(NewLaneRecord {
            id: run_id.clone(),
            lane: MAIN_LANE.to_string(),
            record: LaneRecordEntry::OperationStarted {
                source_leaf_id: self.store.leaf_id(),
                intent: OperationIntent::Run {
                    original_prompt,
                    initial_messages: pending.iter().map(|item| item.target.clone()).collect(),
                    system_prompt_override: None,
                    resume_data: None,
                },
            },
        })?;
        state.active_run = Some(ActiveSessionRun {
            id: run_id.clone(),
            generation,
            pending,
        });
        self.phase.send_replace(ActivityPhase::Running);
        Ok(run_id)
    }

    pub(super) fn enqueue_message(
        &self,
        agent: &Agent,
        message: Message,
        kind: QueueKind,
    ) -> Result<PromptOutcome, SessionError> {
        let display_text = message_display_text(&message);
        let entry = match &message {
            Message::Custom(custom) => SessionEntry::custom_message(custom),
            _ => SessionEntry::message(message.clone()),
        };
        self.enqueue(
            agent,
            None,
            QueuedInput {
                kind,
                display_text,
                message,
                entry,
            },
        )
    }

    pub(super) fn enqueue_text(
        &self,
        agent: &Agent,
        target: Option<LiveDelivery>,
        kind: QueueKind,
        input: QueuedTextOutcome,
    ) -> Result<PromptOutcome, SessionError> {
        let QueuedTextOutcome::Message {
            generation,
            display_text,
            text,
            images,
        } = input
        else {
            return Ok(PromptOutcome::Handled);
        };
        if target
            .as_ref()
            .is_some_and(|target| generation != target.generation)
        {
            return Err(SessionError::Busy);
        }
        let message = Message::User(input_user_message(&text, &images, now_ms()));
        let wire = if display_text == text {
            AgentMessage::from(message.clone())
        } else {
            AgentMessage::with_display_text(message.clone(), &display_text)?
        };
        self.enqueue(
            agent,
            target,
            QueuedInput {
                kind,
                display_text,
                message,
                entry: SessionEntry::message(wire),
            },
        )
    }

    fn enqueue(
        &self,
        agent: &Agent,
        expected: Option<LiveDelivery>,
        input: QueuedInput,
    ) -> Result<PromptOutcome, SessionError> {
        let _dispatch = self.events.defer_observers();
        let mut state = self.lock();
        let idle_delivery =
            input.kind != QueueKind::NextRun && state.active_run.is_none() && expected.is_none();
        let (pending, run_id) = if input.kind == QueueKind::NextRun || idle_delivery {
            (&mut state.recovered_queue, None)
        } else {
            let run = state.active_run.as_mut().ok_or(SessionError::Busy)?;
            if expected.is_some_and(|expected| {
                expected.run_id != run.id || expected.generation != run.generation
            }) {
                return Err(SessionError::Busy);
            }
            (&mut run.pending, Some(run.id.clone()))
        };
        let target = ProvisionedEntry {
            id: next_unique_id("entry"),
            entry: input.entry,
        };
        append_queue_enqueued(
            &self.store,
            if idle_delivery {
                QueueKind::NextRun
            } else {
                input.kind
            },
            run_id.clone(),
            &target,
        )?;
        let entry_id = target.id.clone();
        pending.push(PendingSessionMessage {
            kind: Some(input.kind),
            run_id,
            display_text: input.display_text,
            message: input.message.clone(),
            target,
            deliver_on_prompt: input.kind != QueueKind::NextRun,
            started: false,
        });
        // Keep closing/clearing delivery excluded until the Agent has received
        // this already-durable item. Product queue state precedes consumption.
        self.events.publish_queue(state.queue_snapshot());
        match input.kind {
            _ if idle_delivery => {}
            QueueKind::Steer => agent.enqueue_identified(
                entry_id.clone(),
                input.message,
                pi_agent::QueueDestination::Steering,
            ),
            QueueKind::FollowUp => agent.enqueue_identified(
                entry_id.clone(),
                input.message,
                pi_agent::QueueDestination::FollowUp,
            ),
            QueueKind::NextRun => {}
        }
        Ok(PromptOutcome::Queued {
            kind: input.kind,
            entry_id,
        })
    }

    /// Binds idle queued input to the newly admitted run before the Agent can consume it.
    pub(super) fn activate_idle_queue(&self, agent: &Agent) -> Result<(), SessionError> {
        let _dispatch = self.events.defer_observers();
        let mut state = self.lock();
        let run_id = state
            .active_run
            .as_ref()
            .ok_or(SessionError::Busy)?
            .id
            .clone();
        let mut entries = Vec::new();
        let mut pending = Vec::new();
        for item in state
            .recovered_queue
            .iter()
            .filter(|item| item.deliver_on_prompt)
        {
            entries.push(NewLaneRecord {
                id: next_unique_id("queue-cancel"),
                lane: MAIN_LANE.into(),
                record: LaneRecordEntry::QueueCancelled {
                    run_id: None,
                    entry_id: item.target.id.clone(),
                },
            });
            let mut item = item.clone();
            item.target.id = next_unique_id("entry");
            item.run_id = Some(run_id.clone());
            entries.push(NewLaneRecord {
                id: next_unique_id("queue"),
                lane: MAIN_LANE.into(),
                record: LaneRecordEntry::QueueEnqueued {
                    queue: item.kind.expect("idle input has a queue"),
                    run_id: Some(run_id.clone()),
                    target: item.target.clone(),
                },
            });
            pending.push(item);
        }
        if pending.is_empty() {
            return Ok(());
        }
        self.store.append_records_batch(entries)?;
        state.recovered_queue.retain(|item| !item.deliver_on_prompt);
        state
            .active_run
            .as_mut()
            .expect("admitted run")
            .pending
            .extend(pending.clone());
        // Only the journal identities/run association changed; the ordered
        // steering/follow-up lists are still the same pending input.
        self.events
            .publish_queue_with_origin(state.queue_snapshot(), crate::QueueUpdateOrigin::Retimed);
        for item in pending {
            let destination = match item.kind {
                Some(QueueKind::Steer) => pi_agent::QueueDestination::Steering,
                Some(QueueKind::FollowUp) => pi_agent::QueueDestination::FollowUp,
                _ => unreachable!("idle live delivery is steer or follow-up"),
            };
            agent.enqueue_identified(item.target.id, item.message, destination);
        }
        Ok(())
    }

    pub(super) fn display_text(&self, event: &AgentEvent) -> Option<String> {
        let message = match event {
            AgentEvent::MessageStart {
                message: message @ Message::User(_),
            }
            | AgentEvent::MessageEnd {
                message: message @ Message::User(_),
            } => message,
            _ => return None,
        };
        self.lock()
            .active_run
            .as_ref()?
            .pending
            .iter()
            .find(|pending| pending.started && pending_message_matches(pending, message))
            .and_then(|pending| {
                pending_message_equivalent(pending, message).then(|| pending.display_text.clone())
            })
    }

    /// Publish live consumption before the user start event, without committing
    /// the provisional message or changing its durable recovery identity.
    pub(super) fn message_started(
        &self,
        message: &Message,
        agent: &pi_agent::WeakAgent,
    ) -> Option<QueueSnapshot> {
        if !matches!(message, Message::User(_)) {
            return None;
        }
        let mut state = self.lock();
        // Enqueue/edit also locks activity before Agent queues. While this
        // listener runs, the drained batch cannot advance to another message.
        let queued = agent.queued_message_ids()?;
        let pending = state
            .active_run
            .as_mut()?
            .pending
            .iter_mut()
            .find(|pending| {
                !pending.started
                    && !queued.contains(&pending.target.id)
                    && pending_message_matches(pending, message)
            })?;
        pending.started = true;
        pending.kind.is_some().then(|| state.queue_snapshot())
    }

    /// A pending item is acknowledged only after its final hook-patched message
    /// has committed. Provisioned identities and lazy materialization stay here.
    pub(super) fn message_ended(
        &self,
        message: &Message,
        active_tools: Option<&[String]>,
    ) -> Result<MessageCommit, SessionError> {
        let mut state = self.lock();
        let selection = active_tools
            .map(|tools| {
                let unchanged = self.store.selected_active_tools()?.as_deref() == Some(tools);
                Ok::<_, SessionError>((!unchanged).then(|| {
                    SessionEntry::ActiveToolsChange(crate::ActiveToolsEntry {
                        active_tool_names: tools.to_vec(),
                    })
                }))
            })
            .transpose()?
            .flatten();
        let mut configuration = None;
        let matched = state.active_run.as_ref().and_then(|run| {
            run.pending
                .iter()
                .position(|pending| pending.started && pending_message_matches(pending, message))
                .or_else(|| {
                    run.pending
                        .iter()
                        .position(|pending| pending_message_matches(pending, message))
                })
        });
        let (entry, queue) = if let Some(index) = matched {
            let run = state
                .active_run
                .as_mut()
                .expect("matched input has an active run");
            let pending = &run.pending[index];
            let mut target = pending.target.clone();
            if !pending_message_equivalent(pending, message) {
                target.entry = match message {
                    Message::Custom(custom) => SessionEntry::custom_message(custom),
                    message => SessionEntry::message(message.clone()),
                };
                {
                    self.store.append_record(NewLaneRecord {
                        id: next_unique_id("message-replacement"),
                        lane: MAIN_LANE.to_string(),
                        record: LaneRecordEntry::WriteDeferred {
                            run_id: run.id.clone(),
                            target: target.clone(),
                        },
                    })?;
                }
            }
            let record = self.store.append_entry(target, MAIN_LANE)?;
            let pending = run.pending.remove(index);
            (
                record,
                (pending.kind.is_some() && !pending.started).then(|| state.queue_snapshot()),
            )
        } else {
            let entry = match message {
                Message::Custom(custom) => SessionEntry::custom_message(custom),
                message => SessionEntry::message(message.clone()),
            };
            if let Some(selection) = selection {
                // The tool result and loadout share the normal journal batch.
                // A resumed or branched session restores the selection record,
                // never an inferred union of historical tool-result metadata.
                let mut records = self.store.append_batch_records(vec![entry, selection])?;
                configuration = records.pop();
                (records.pop().expect("message batch has a result"), None)
            } else {
                (self.store.append_session_record(entry)?, None)
            }
        };
        if matches!(message, Message::Assistant(_)) {
            self.store.materialize()?;
        }
        Ok(MessageCommit {
            entry,
            queue,
            configuration,
        })
    }
    /// Clears queued messages and returns their editor-ready text. The
    /// cancellation records make the same result survive process recovery.
    pub(super) fn clear_queue(&self, agent: &Agent) -> Result<QueueSnapshot, SessionError> {
        let _dispatch = self.events.defer_observers();
        let mut activity = self.lock();
        if activity.active_run.as_ref().is_some_and(|run| {
            run.pending
                .iter()
                .any(|item| !item.started && item.kind == Some(QueueKind::NextRun))
        }) {
            return Err(SessionError::Busy);
        }
        let cleared = activity.queue_snapshot();
        let cancellations = activity
            .recovered_queue
            .iter()
            .chain(
                activity
                    .active_run
                    .iter()
                    .flat_map(|run| run.pending.iter()),
            )
            .filter(|item| !item.started && item.kind.is_some())
            .map(|item| NewLaneRecord {
                id: next_unique_id("queue-cancel"),
                lane: MAIN_LANE.to_string(),
                record: LaneRecordEntry::QueueCancelled {
                    run_id: item.run_id.clone(),
                    entry_id: item.target.id.clone(),
                },
            });
        let ids = |kind| {
            activity
                .active_run
                .iter()
                .flat_map(|run| &run.pending)
                .filter(|item| !item.started && item.kind == Some(kind))
                .map(|item| item.target.id.clone())
                .collect::<Vec<_>>()
        };
        let steering = ids(QueueKind::Steer);
        let follow_up = ids(QueueKind::FollowUp);
        let committed = agent.replace_identified_queue(
            pi_agent::QueueDestination::Steering,
            &steering,
            Vec::new(),
            || {
                let committed = agent.replace_identified_queue(
                    pi_agent::QueueDestination::FollowUp,
                    &follow_up,
                    Vec::new(),
                    || {
                        self.store
                            .append_records_batch(cancellations)
                            .map(|_| ())
                            .map_err(SessionError::from)
                    },
                )?;
                if committed {
                    Ok(())
                } else {
                    Err(SessionError::Busy)
                }
            },
        )?;
        if !committed {
            return Err(SessionError::Busy);
        }
        activity.recovered_queue.clear();
        if let Some(run) = &mut activity.active_run {
            run.pending
                .retain(|item| item.started || item.kind.is_none());
        }
        let snapshot = activity.queue_snapshot();
        self.events.publish_queue(snapshot);
        Ok(cleared)
    }

    pub(super) fn finish_run(
        &self,
        agent: &Agent,
        run_id: &str,
        result: &Result<AgentLoopOutcome, SessionError>,
    ) -> Result<(), SessionError> {
        self.phase.send_replace(ActivityPhase::Settling);
        let snapshot = {
            let mut activity = self.lock();
            let before = activity.queue_snapshot();
            let Some(run) = activity.active_run.take() else {
                return Err(SessionError::Runtime(
                    "active session operation disappeared before completion".to_string(),
                ));
            };
            if run.id != run_id {
                activity.active_run = Some(run);
                return Err(SessionError::Runtime(
                    "active session operation id changed before completion".to_string(),
                ));
            }
            let mut restored_uncommitted_delivery = false;
            for mut item in run.pending.into_iter().filter(|item| item.kind.is_some()) {
                restored_uncommitted_delivery |= item.started;
                requeue_for_next_run(&self.store, &mut item)?;
                item.started = false;
                activity.recovered_queue.push(item);
            }
            agent.clear_all_queues();
            let after = activity.queue_snapshot();
            let origin = if restored_uncommitted_delivery {
                // A failed delivery becomes visible again; this is not only an
                // identity rebind of an already visible pending suffix.
                crate::QueueUpdateOrigin::Mutation
            } else {
                crate::QueueUpdateOrigin::Retimed
            };
            (after != before).then_some((after, origin))
        };
        if let Some((snapshot, origin)) = snapshot {
            // Classify the operation, never equality of user payloads.
            self.events.publish_queue_with_origin(snapshot, origin);
        }

        let (outcome, error) = match result {
            Ok(outcome) => match outcome.stop {
                AgentLoopStop::Aborted => (OperationOutcome::Aborted, None),
                AgentLoopStop::ProviderError | AgentLoopStop::MaxToolIterations => (
                    OperationOutcome::Failed,
                    Some(OperationError {
                        code: "agent_run_failed".to_string(),
                        message: format!("agent run stopped with {:?}", outcome.stop),
                    }),
                ),
                AgentLoopStop::Completed | AgentLoopStop::TerminatedByTools => {
                    (OperationOutcome::Completed, None)
                }
            },
            Err(error) => (
                OperationOutcome::Failed,
                Some(OperationError {
                    code: "session_run_failed".to_string(),
                    message: error.to_string(),
                }),
            ),
        };
        self.store.append_record(NewLaneRecord {
            id: next_unique_id("run-finish"),
            lane: MAIN_LANE.to_string(),
            record: LaneRecordEntry::OperationFinished {
                run_id: run_id.to_string(),
                outcome,
                error,
            },
        })?;
        Ok(())
    }

    pub(super) async fn begin(&self, phase: ActivityPhase) -> ActivityGuard<'_> {
        let gate = self.gate.lock().await;
        self.prepare_run();
        self.phase.send_replace(phase);
        ActivityGuard {
            activity: self,
            _gate: gate,
        }
    }

    pub(super) fn try_begin(&self) -> Result<ActivityGuard<'_>, SessionError> {
        let gate = self.gate.try_lock().map_err(|_| SessionError::Busy)?;
        self.phase.send_replace(ActivityPhase::Updating);
        Ok(ActivityGuard {
            activity: self,
            _gate: gate,
        })
    }

    pub(super) fn is_idle(&self) -> bool {
        *self.phase.borrow() == ActivityPhase::Idle
    }

    pub(super) fn is_summarizing(&self) -> bool {
        matches!(
            *self.phase.borrow(),
            ActivityPhase::Compacting | ActivityPhase::Summarizing
        )
    }

    pub(super) async fn wait_for_idle(&self) {
        let mut phase = self.phase.subscribe();
        while *phase.borrow_and_update() != ActivityPhase::Idle {
            if phase.changed().await.is_err() {
                break;
            }
        }
    }

    pub(super) fn phase(&self, phase: ActivityPhase) -> PhaseGuard<'_> {
        let previous = self.phase.send_replace(phase);
        PhaseGuard {
            activity: self,
            previous,
        }
    }

    fn prepare_run(&self) {
        self.aborted.store(false, Ordering::Release);
        self.stopped.store(false, Ordering::Release);
        self.continuation.store(false, Ordering::Release);
    }

    pub(super) fn accepts_live_messages(&self) -> bool {
        self.lock().active_run.is_some()
    }

    pub(super) fn has_pending_messages(&self) -> bool {
        let state = self.lock();
        !state.recovered_queue.is_empty()
            || state
                .active_run
                .as_ref()
                .is_some_and(|run| run.pending.iter().any(|item| item.kind.is_some()))
    }

    pub(super) fn abort(&self) {
        self.aborted.store(true, Ordering::Release);
        let cancellation = self
            .cancellation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for handle in [
            &cancellation.retry,
            &cancellation.compaction,
            &cancellation.boundary,
        ]
        .into_iter()
        .flatten()
        {
            handle.abort();
        }
    }

    pub(super) fn is_aborted(&self) -> bool {
        self.aborted.load(Ordering::Acquire)
    }

    pub(super) fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
    }
    pub(super) fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }
    pub(super) fn request_continuation(&self, requested: bool) {
        self.continuation.store(requested, Ordering::Release);
    }
    pub(super) fn take_continuation(&self) -> bool {
        self.continuation.swap(false, Ordering::AcqRel)
    }
    pub(super) fn retry_attempt(&self) -> u32 {
        self.retry_attempt.load(Ordering::Acquire)
    }
    pub(super) fn set_retry_attempt(&self, attempt: u32) {
        self.retry_attempt.store(attempt, Ordering::Release);
    }

    pub(super) fn register_cancellation(&self, stage: CancellationStage, handle: AbortHandle) {
        let mut cancellation = self
            .cancellation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.is_aborted() {
            handle.abort();
        }
        *cancellation.slot(stage) = Some(handle);
    }

    pub(super) fn clear_cancellation(&self, stage: CancellationStage) {
        self.cancellation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .slot(stage)
            .take();
    }

    pub(super) fn abort_stage(&self, stage: CancellationStage) {
        if let Some(handle) = self
            .cancellation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .slot(stage)
        {
            handle.abort();
        }
    }

    pub(super) fn register_shell(
        &self,
        id: String,
        handle: AbortHandle,
    ) -> Result<(), SessionError> {
        let mut cancellation = self
            .cancellation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if cancellation.shell.is_some() {
            return Err(SessionError::Busy);
        }
        cancellation.shell = Some((id, handle));
        Ok(())
    }

    pub(super) fn abort_shell(&self) {
        if let Some((_, handle)) = &self
            .cancellation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .shell
        {
            handle.abort();
        }
    }

    pub(super) fn clear_shell(&self, id: &str) {
        let mut cancellation = self
            .cancellation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if cancellation
            .shell
            .as_ref()
            .is_some_and(|(current, _)| current == id)
        {
            cancellation.shell.take();
        }
    }
}

pub(super) struct ActivityGuard<'a> {
    activity: &'a SessionActivity,
    _gate: tokio::sync::MutexGuard<'a, ()>,
}

impl Drop for ActivityGuard<'_> {
    fn drop(&mut self) {
        self.activity.set_retry_attempt(0);
        self.activity.phase.send_replace(ActivityPhase::Idle);
    }
}

pub(super) struct PhaseGuard<'a> {
    activity: &'a SessionActivity,
    previous: ActivityPhase,
}

impl Drop for PhaseGuard<'_> {
    fn drop(&mut self) {
        self.activity.phase.send_replace(self.previous);
    }
}

struct QueuedInput {
    kind: QueueKind,
    display_text: String,
    message: Message,
    entry: SessionEntry,
}

/// Opaque identity captured before async input preprocessing.
pub(super) struct LiveDelivery {
    run_id: String,
    generation: u64,
}

/// Recovery can transfer pending input without exposing the live queue representation.
#[derive(Default)]
pub(super) struct RecoveredQueue(Vec<PendingSessionMessage>);

impl RecoveredQueue {
    pub(super) fn snapshot(&self) -> QueueSnapshot {
        queue_snapshot(self.0.iter())
    }

    pub(super) fn recover(
        store: &SessionStore,
        records: &[LaneRecord],
        missing: Vec<ProvisionedEntry>,
    ) -> Result<Self, SessionError> {
        let mut recovered = recover_pending_queue(store, records);
        for item in &mut recovered {
            if item.run_id.is_some() {
                requeue_for_next_run(store, item)?;
            }
        }
        for target in missing {
            if recovered.iter().any(|item| item.target.id == target.id) {
                continue;
            }
            append_queue_enqueued(store, QueueKind::NextRun, None, &target)?;
            if let Some(item) = pending_session_message(QueueKind::NextRun, None, target, None) {
                recovered.push(item);
            }
        }
        Ok(Self(recovered))
    }
}

pub(super) struct MessageCommit {
    pub(super) entry: SessionRecord,
    pub(super) queue: Option<QueueSnapshot>,
    pub(super) configuration: Option<SessionRecord>,
}

fn input_user_message(text: &str, images: &[ImageContent], timestamp_ms: i64) -> UserMessage {
    let mut content = vec![ContentBlock::Text(pi_core::TextContent::new(text))];
    content.extend(images.iter().cloned().map(ContentBlock::Image));
    UserMessage {
        content,
        timestamp_ms,
    }
}

fn message_display_text(message: &Message) -> String {
    let blocks = match message {
        Message::User(message) => message.content.as_slice(),
        Message::Custom(message) => match &message.content {
            pi_core::CustomMessageContent::Text(text) => return text.clone(),
            pi_core::CustomMessageContent::Blocks(blocks) => blocks.as_slice(),
        },
        Message::System(_) | Message::Assistant(_) | Message::ToolResult(_) => {
            return String::new();
        }
    };
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn pending_message_matches(pending: &PendingSessionMessage, message: &Message) -> bool {
    match (&pending.message, message) {
        (Message::User(_), Message::User(_)) => true,
        // Tool fields on a caller-supplied pending system message are normalized
        // by the Agent. Retain its identity while committing the normalized value.
        (Message::System(expected), Message::System(actual)) => {
            expected.content == actual.content
                && expected.sections == actual.sections
                && expected.timestamp_ms == actual.timestamp_ms
        }
        _ => pending.message == *message,
    }
}

fn pending_message_equivalent(pending: &PendingSessionMessage, message: &Message) -> bool {
    match (&pending.message, message) {
        (Message::User(expected), Message::User(actual)) => expected.content == actual.content,
        _ => pending.message == *message,
    }
}

fn append_queue_enqueued(
    store: &SessionStore,
    kind: QueueKind,
    run_id: Option<String>,
    target: &ProvisionedEntry,
) -> Result<(), SessionError> {
    store.append_record(NewLaneRecord {
        id: next_unique_id("queue"),
        lane: MAIN_LANE.to_string(),
        record: LaneRecordEntry::QueueEnqueued {
            queue: kind,
            run_id,
            target: target.clone(),
        },
    })?;
    Ok(())
}

fn requeue_for_next_run(
    store: &SessionStore,
    item: &mut PendingSessionMessage,
) -> Result<(), SessionError> {
    store.append_record(NewLaneRecord {
        id: next_unique_id("queue-cancel"),
        lane: MAIN_LANE.to_string(),
        record: LaneRecordEntry::QueueCancelled {
            run_id: item.run_id.clone(),
            entry_id: item.target.id.clone(),
        },
    })?;
    item.target.id = next_unique_id("entry");
    item.run_id = None;
    append_queue_enqueued(store, QueueKind::NextRun, None, &item.target)
}

fn pending_session_message(
    kind: QueueKind,
    run_id: Option<String>,
    target: ProvisionedEntry,
    timestamp_ms: Option<i64>,
) -> Option<PendingSessionMessage> {
    let (message, display_text) = match &target.entry {
        SessionEntry::Message(message_entry) => {
            let message = message_entry.message.as_standard()?.clone();
            if !matches!(message, Message::User(_)) {
                return None;
            }
            let display_text = message_entry
                .message
                .display_text()
                .map(str::to_string)
                .unwrap_or_else(|| message_display_text(&message));
            (message, display_text)
        }
        SessionEntry::CustomMessage(custom) => {
            let message = custom.to_message(timestamp_ms.unwrap_or_else(now_ms));
            let display_text = message_display_text(&message);
            (message, display_text)
        }
        _ => return None,
    };
    Some(PendingSessionMessage {
        kind: Some(kind),
        run_id,
        display_text,
        message,
        target,
        deliver_on_prompt: false,
        started: false,
    })
}

fn recover_pending_queue(
    store: &SessionStore,
    records: &[LaneRecord],
) -> Vec<PendingSessionMessage> {
    let cancelled = records
        .iter()
        .filter_map(|record| match &record.record {
            LaneRecordEntry::QueueCancelled { entry_id, .. } => Some(entry_id.as_str()),
            _ => None,
        })
        .collect::<std::collections::HashSet<_>>();
    records
        .iter()
        .filter_map(|record| {
            let LaneRecordEntry::QueueEnqueued {
                queue,
                run_id,
                target,
            } = &record.record
            else {
                return None;
            };
            if store.get_entry(&target.id).is_some() || cancelled.contains(target.id.as_str()) {
                return None;
            }
            pending_session_message(
                *queue,
                run_id.clone(),
                target.clone(),
                Some(record.timestamp_ms),
            )
        })
        .collect()
}

fn queue_snapshot<'a>(items: impl IntoIterator<Item = &'a PendingSessionMessage>) -> QueueSnapshot {
    let mut snapshot = QueueSnapshot::default();
    for item in items.into_iter().filter(|item| !item.started) {
        if let Some(kind) = item.kind {
            snapshot.items.push(crate::QueuedMessageSnapshot {
                entry_id: item.target.id.clone(),
                kind: if item.run_id.is_none() && !item.deliver_on_prompt {
                    QueueKind::NextRun
                } else {
                    kind
                },
                text: item.display_text.clone(),
            });
        }
        match item.kind {
            Some(QueueKind::Steer) => snapshot.steering.push(item.display_text.clone()),
            Some(QueueKind::FollowUp | QueueKind::NextRun) => {
                snapshot.follow_up.push(item.display_text.clone());
            }
            None => {}
        }
    }
    snapshot
}
