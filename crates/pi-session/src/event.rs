use std::sync::{Arc, RwLock};

use pi_agent::AgentStateSnapshot;
use pi_core::{AgentEvent, Message, ThinkingLevel};
use pi_shell::{ShellResult, ShellStream};
use tokio::sync::broadcast;

use crate::{CompactionReason, SessionRecord};

const DEFAULT_EVENT_CAPACITY: usize = 512;

/// Presentation-neutral severity attached to a JavaScript extension notice.
///
/// Frontends decide how to render the notice; extensions never receive a
/// terminal handle through this event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExtensionNoticeLevel {
    Info,
    Warning,
    Error,
}

impl ExtensionNoticeLevel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

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
    SessionInfoChanged {
        name: Option<String>,
    },
    ExtensionNotice {
        message: String,
        level: ExtensionNoticeLevel,
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

    pub(crate) fn publish_extension_notice(&self, message: String, level: ExtensionNoticeLevel) {
        self.publish(
            AgentSessionEvent::ExtensionNotice { message, level },
            |_| {},
        );
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
        let revision = {
            let mut snapshot = self
                .snapshot
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            update(&mut snapshot);
            snapshot.revision = snapshot.revision.saturating_add(1);
            snapshot.revision
        };
        let _ = self
            .sender
            .send(RevisionedAgentSessionEvent { revision, event });
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use pi_core::{ModelId, ProviderId};

    use super::*;

    fn agent_state() -> AgentStateSnapshot {
        AgentStateSnapshot {
            system_prompt: String::new(),
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            thinking_level: ThinkingLevel::Off,
            active_tools: Vec::new(),
            messages: Vec::new(),
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
}
