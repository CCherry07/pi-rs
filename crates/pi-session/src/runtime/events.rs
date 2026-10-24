use std::sync::{Arc, RwLock};

use pi_agent::AgentStateSnapshot;
use pi_core::{AgentEvent, Message, ThinkingLevel, Usage};
use pi_plugin::NoticeLevel;
use pi_shell::{ShellResult, ShellStream};
use tokio::sync::broadcast;

use crate::{CompactionReason, SessionRecord};

const DEFAULT_EVENT_CAPACITY: usize = 512;

/// Product events emitted by an [`crate::AgentSession`].
///
/// This is the Rust counterpart of Pi's `AgentSessionEvent`: core agent
/// lifecycle events are combined with session-owned queue, compaction,
/// persistence, and configuration events. Runtime replacement remains a
/// separate concern of [`crate::PiSession`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum AgentSessionEvent {
    Agent(Box<AgentEvent>),
    /// Session-authoritative agent end. The low-level agent cannot know
    /// whether the session retry policy will continue the run.
    AgentEnd {
        messages: Vec<Message>,
        will_retry: bool,
    },
    AgentSettled,
    QueueUpdate {
        steering: Vec<String>,
        follow_up: Vec<String>,
    },
    CompactionStart {
        reason: CompactionReason,
    },
    CompactionEnd {
        reason: CompactionReason,
        /// The committed compaction record. Keeping its tree identity lets
        /// Pi JSON/RPC consumers recover `firstKeptEntryId` exactly.
        result: Option<SessionRecord>,
        aborted: bool,
        will_retry: bool,
        error_message: Option<String>,
    },
    AutoRetryStart {
        attempt: u32,
        max_attempts: u32,
        delay_ms: u64,
        error_message: String,
    },
    AutoRetryEnd {
        success: bool,
        attempt: u32,
        final_error: Option<String>,
    },
    EntryAppended {
        entry: SessionRecord,
    },
    /// Detached provider usage was added to the session ledger without a
    /// corresponding conversation entry.
    UsageRecorded {
        usage: Usage,
    },
    SessionInfoChanged {
        name: Option<String>,
    },
    PluginNotice {
        message: String,
        level: NoticeLevel,
    },
    ThinkingLevelChanged {
        level: ThinkingLevel,
    },
    BashExecutionStart {
        id: String,
        command: String,
        exclude_from_context: bool,
    },
    BashExecutionUpdate {
        id: String,
        stream: ShellStream,
        delta: String,
    },
    BashExecutionEnd {
        id: String,
        result: Option<ShellResult>,
        error_message: Option<String>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueueSnapshot {
    pub steering: Vec<String>,
    pub follow_up: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionSnapshot {
    pub reason: CompactionReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashExecutionSnapshot {
    pub id: String,
    pub command: String,
    pub exclude_from_context: bool,
    pub output: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoRetrySnapshot {
    pub attempt: u32,
    pub max_attempts: u32,
    pub delay_ms: u64,
    pub error_message: String,
}

/// Authoritative state used to initialize a frontend or recover after a
/// lagged broadcast receiver.
#[derive(Debug, Clone)]
pub struct AgentSessionSnapshot {
    pub revision: u64,
    pub agent: AgentStateSnapshot,
    pub queue: QueueSnapshot,
    pub compaction: Option<CompactionSnapshot>,
    pub auto_retry: Option<AutoRetrySnapshot>,
    pub bash: Option<BashExecutionSnapshot>,
    pub name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RevisionedAgentSessionEvent {
    pub revision: u64,
    pub event: AgentSessionEvent,
}

/// A race-free initial snapshot plus the subsequent ordered event stream.
///
/// Receivers that observe `RecvError::Lagged` must call
/// [`crate::AgentSession::snapshot`] and resume after that revision.
pub struct AgentSessionSubscription {
    pub snapshot: AgentSessionSnapshot,
    pub events: broadcast::Receiver<RevisionedAgentSessionEvent>,
}

pub(crate) struct AgentSessionEventHub {
    snapshot: RwLock<AgentSessionSnapshot>,
    sender: broadcast::Sender<RevisionedAgentSessionEvent>,
}

impl AgentSessionEventHub {
    pub(crate) fn new(
        agent: AgentStateSnapshot,
        name: Option<String>,
        queue: QueueSnapshot,
    ) -> Arc<Self> {
        let (sender, _) = broadcast::channel(DEFAULT_EVENT_CAPACITY);
        Arc::new(Self {
            snapshot: RwLock::new(AgentSessionSnapshot {
                revision: 0,
                agent,
                queue,
                compaction: None,
                auto_retry: None,
                bash: None,
                name,
            }),
            sender,
        })
    }

    pub(crate) fn snapshot(&self) -> AgentSessionSnapshot {
        self.snapshot
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Navigation has a direct result and lifecycle hook, rather than a wire
    /// event. Advance the snapshot cursor so previously queued events cannot
    /// restore a context from before that navigation.
    pub(crate) fn refresh_agent(&self, agent: AgentStateSnapshot) {
        let mut snapshot = self
            .snapshot
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        snapshot.agent = agent;
        snapshot.revision = snapshot.revision.saturating_add(1);
    }

    pub(crate) fn subscribe(&self) -> AgentSessionSubscription {
        // Subscribe first. If a publish races before the snapshot read, the
        // snapshot already contains it and the receiver contains a harmless
        // duplicate revision. If it races after, the receiver contains the
        // next revision.
        let events = self.sender.subscribe();
        let snapshot = self.snapshot();
        AgentSessionSubscription { snapshot, events }
    }

    pub(crate) fn publish_agent(&self, event: AgentEvent, agent: AgentStateSnapshot) {
        self.publish(AgentSessionEvent::Agent(Box::new(event)), |snapshot| {
            snapshot.agent = agent;
        });
    }

    pub(crate) fn publish_agent_end(
        &self,
        messages: Vec<Message>,
        will_retry: bool,
        agent: AgentStateSnapshot,
    ) {
        self.publish(
            AgentSessionEvent::AgentEnd {
                messages,
                will_retry,
            },
            |snapshot| snapshot.agent = agent,
        );
    }

    pub(crate) fn publish_agent_settled(&self, agent: AgentStateSnapshot) {
        self.publish(AgentSessionEvent::AgentSettled, |snapshot| {
            snapshot.agent = agent;
        });
    }

    pub(crate) fn publish_entry(&self, entry: SessionRecord) {
        self.publish(
            AgentSessionEvent::EntryAppended {
                entry: entry.clone(),
            },
            |_| {},
        );
    }

    pub(crate) fn publish_initialized_context(
        &self,
        entry: SessionRecord,
        agent: AgentStateSnapshot,
    ) {
        self.publish(AgentSessionEvent::EntryAppended { entry }, |snapshot| {
            snapshot.agent = agent
        });
    }

    pub(crate) fn publish_usage(&self, usage: Usage) {
        self.publish(AgentSessionEvent::UsageRecorded { usage }, |_| {});
    }

    pub(crate) fn publish_configuration(
        &self,
        entries: Vec<SessionRecord>,
        agent: AgentStateSnapshot,
        thinking_level: Option<ThinkingLevel>,
    ) {
        for entry in entries {
            self.publish_initialized_context(entry, agent.clone());
        }
        if let Some(thinking_level) = thinking_level {
            self.publish_thinking(thinking_level, agent);
        }
    }

    pub(crate) fn publish_queue(&self, queue: QueueSnapshot) {
        self.publish(
            AgentSessionEvent::QueueUpdate {
                steering: queue.steering.clone(),
                follow_up: queue.follow_up.clone(),
            },
            |snapshot| snapshot.queue = queue,
        );
    }

    pub(crate) fn publish_compaction_start(&self, reason: CompactionReason) {
        self.publish(AgentSessionEvent::CompactionStart { reason }, |snapshot| {
            snapshot.compaction = Some(CompactionSnapshot { reason });
        });
    }

    pub(crate) fn publish_compaction_end(
        &self,
        reason: CompactionReason,
        result: Option<SessionRecord>,
        aborted: bool,
        will_retry: bool,
        error_message: Option<String>,
    ) {
        self.publish(
            AgentSessionEvent::CompactionEnd {
                reason,
                result,
                aborted,
                will_retry,
                error_message,
            },
            |snapshot| snapshot.compaction = None,
        );
    }

    pub(crate) fn publish_auto_retry_start(
        &self,
        attempt: u32,
        max_attempts: u32,
        delay_ms: u64,
        error_message: String,
    ) {
        self.publish(
            AgentSessionEvent::AutoRetryStart {
                attempt,
                max_attempts,
                delay_ms,
                error_message: error_message.clone(),
            },
            |snapshot| {
                snapshot.auto_retry = Some(AutoRetrySnapshot {
                    attempt,
                    max_attempts,
                    delay_ms,
                    error_message,
                });
            },
        );
    }

    pub(crate) fn publish_auto_retry_end(
        &self,
        success: bool,
        attempt: u32,
        final_error: Option<String>,
    ) {
        self.publish(
            AgentSessionEvent::AutoRetryEnd {
                success,
                attempt,
                final_error,
            },
            |snapshot| snapshot.auto_retry = None,
        );
    }

    pub(crate) fn publish_session_info(&self, name: Option<String>) {
        self.publish(
            AgentSessionEvent::SessionInfoChanged { name: name.clone() },
            |snapshot| snapshot.name = name,
        );
    }

    pub(crate) fn publish_extension_notice(&self, message: String, level: NoticeLevel) {
        self.publish(AgentSessionEvent::PluginNotice { message, level }, |_| {});
    }

    pub(crate) fn publish_thinking(&self, level: ThinkingLevel, agent: AgentStateSnapshot) {
        self.publish(
            AgentSessionEvent::ThinkingLevelChanged { level },
            |snapshot| snapshot.agent = agent,
        );
    }

    pub(crate) fn publish_bash_start(
        &self,
        id: String,
        command: String,
        exclude_from_context: bool,
    ) {
        self.publish(
            AgentSessionEvent::BashExecutionStart {
                id: id.clone(),
                command: command.clone(),
                exclude_from_context,
            },
            |snapshot| {
                snapshot.bash = Some(BashExecutionSnapshot {
                    id,
                    command,
                    exclude_from_context,
                    output: String::new(),
                });
            },
        );
    }

    pub(crate) fn publish_bash_update(&self, id: String, stream: ShellStream, delta: String) {
        self.publish(
            AgentSessionEvent::BashExecutionUpdate {
                id: id.clone(),
                stream,
                delta: delta.clone(),
            },
            |snapshot| {
                if let Some(active) = &mut snapshot.bash
                    && active.id == id
                {
                    active.output.push_str(&delta);
                    if active.output.len() > pi_shell::MAX_OUTPUT_BYTES {
                        let mut boundary = active.output.len() - pi_shell::MAX_OUTPUT_BYTES;
                        while !active.output.is_char_boundary(boundary) {
                            boundary += 1;
                        }
                        active.output.drain(..boundary);
                    }
                }
            },
        );
    }

    pub(crate) fn publish_bash_end(
        &self,
        id: String,
        result: Option<ShellResult>,
        error_message: Option<String>,
    ) {
        self.publish(
            AgentSessionEvent::BashExecutionEnd {
                id,
                result,
                error_message,
            },
            |snapshot| snapshot.bash = None,
        );
    }

    fn publish(&self, event: AgentSessionEvent, update: impl FnOnce(&mut AgentSessionSnapshot)) {
        let mut snapshot = self
            .snapshot
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        update(&mut snapshot);
        snapshot.revision = snapshot.revision.saturating_add(1);
        // Revision assignment and publication have the same order, including
        // concurrent queue, shell and plugin producers.
        let _ = self.sender.send(RevisionedAgentSessionEvent {
            revision: snapshot.revision,
            event,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::{ModelId, ProviderId};
    use std::collections::HashSet;

    fn agent_state() -> AgentStateSnapshot {
        AgentStateSnapshot {
            system_prompt: String::new(),
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            thinking_level: ThinkingLevel::Off,
            active_tools: Vec::new(),
            messages: Arc::default(),
            is_running: false,
            streaming_message: None,
            pending_tool_calls: HashSet::new(),
            error_message: None,
        }
    }

    #[tokio::test]
    async fn subscription_starts_with_an_authoritative_snapshot_then_live_revisions() {
        let hub = AgentSessionEventHub::new(
            agent_state(),
            Some("before".to_string()),
            QueueSnapshot::default(),
        );
        hub.publish_queue(QueueSnapshot {
            steering: vec!["queued".to_string()],
            follow_up: Vec::new(),
        });

        let mut subscription = hub.subscribe();
        assert_eq!(subscription.snapshot.revision, 1);
        assert_eq!(subscription.snapshot.queue.steering, vec!["queued"]);

        hub.publish_session_info(Some("after".to_string()));
        let event = subscription.events.recv().await.unwrap();
        assert_eq!(event.revision, 2);
        assert!(matches!(
            event.event,
            AgentSessionEvent::SessionInfoChanged { name } if name.as_deref() == Some("after")
        ));
        let current = hub.snapshot();
        assert_eq!(current.revision, 2);
        assert_eq!(current.name.as_deref(), Some("after"));
    }

    #[tokio::test]
    async fn unsubscribed_receivers_do_not_affect_snapshot_publication() {
        let hub = AgentSessionEventHub::new(agent_state(), None, QueueSnapshot::default());
        let subscription = hub.subscribe();
        drop(subscription);

        hub.publish_thinking(
            ThinkingLevel::High,
            AgentStateSnapshot {
                thinking_level: ThinkingLevel::High,
                ..agent_state()
            },
        );

        let snapshot = hub.snapshot();
        assert_eq!(snapshot.revision, 1);
        assert_eq!(snapshot.agent.thinking_level, ThinkingLevel::High);
    }

    #[test]
    fn activity_projection_tracks_published_queue_and_compaction() {
        let hub = AgentSessionEventHub::new(agent_state(), None, QueueSnapshot::default());
        assert!(hub.snapshot().compaction.is_none());
        assert!(hub.snapshot().queue.steering.is_empty());

        hub.publish_queue(QueueSnapshot {
            steering: vec!["queued".to_string()],
            follow_up: Vec::new(),
        });
        assert_eq!(hub.snapshot().queue.steering, ["queued"]);

        hub.publish_compaction_start(CompactionReason::Manual);
        assert!(hub.snapshot().compaction.is_some());
        hub.publish_compaction_end(CompactionReason::Manual, None, false, false, None);
        assert!(hub.snapshot().compaction.is_none());
    }

    #[test]
    fn concurrent_publishers_preserve_revision_and_snapshot_order() {
        const WORKERS: usize = 8;
        const PER_WORKER: usize = 512;
        let mut hub = AgentSessionEventHub::new(agent_state(), None, QueueSnapshot::default());
        Arc::get_mut(&mut hub).unwrap().sender = broadcast::channel(WORKERS * PER_WORKER).0;
        let mut subscription = hub.subscribe();
        let barrier = std::sync::Barrier::new(WORKERS);
        std::thread::scope(|scope| {
            for worker in 0..WORKERS {
                let hub = &hub;
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    for index in 0..PER_WORKER {
                        hub.publish_session_info(Some(format!("{worker}:{index}")));
                    }
                });
            }
        });
        let mut last_name = None;
        for revision in 1..=(WORKERS * PER_WORKER) as u64 {
            let event = subscription.events.try_recv().unwrap();
            assert_eq!(event.revision, revision);
            let AgentSessionEvent::SessionInfoChanged { name } = event.event else {
                panic!("unexpected event");
            };
            last_name = name;
        }
        assert!(subscription.events.try_recv().is_err());
        let snapshot = hub.snapshot();
        assert_eq!(snapshot.revision, (WORKERS * PER_WORKER) as u64);
        assert_eq!(snapshot.name, last_name);
    }
}
