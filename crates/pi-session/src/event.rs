use std::sync::{Arc, RwLock};

use pi_agent::AgentStateSnapshot;
use pi_core::{AgentEvent, ThinkingLevel};
use pi_shell::{ShellResult, ShellStream};
use tokio::sync::broadcast;

use crate::{CompactionEntry, CompactionReason, SessionEntry};

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
        result: Option<CompactionEntry>,
        aborted: bool,
        will_retry: bool,
        error_message: Option<String>,
    },
    EntryAppended {
        entry: SessionEntry,
    },
    SessionInfoChanged {
        name: Option<String>,
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

/// Authoritative state used to initialize a frontend or recover after a
/// lagged broadcast receiver.
#[derive(Debug, Clone)]
pub struct AgentSessionSnapshot {
    pub revision: u64,
    pub agent: AgentStateSnapshot,
    pub queue: QueueSnapshot,
    pub compaction: Option<CompactionSnapshot>,
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

    pub(crate) fn publish_agent_settled(&self, agent: AgentStateSnapshot) {
        self.publish(AgentSessionEvent::AgentSettled, |snapshot| {
            snapshot.agent = agent;
        });
    }

    pub(crate) fn publish_entry(&self, entry: SessionEntry) {
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
        result: Option<CompactionEntry>,
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

    pub(crate) fn publish_session_info(&self, name: Option<String>) {
        self.publish(
            AgentSessionEvent::SessionInfoChanged { name: name.clone() },
            |snapshot| snapshot.name = name,
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
