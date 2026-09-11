//! Child presentation keeps the inherited seed separate from the runtime context.
//! Masking (rather than removing) messages preserves the live projection's ordinals.

use pi_core::{CustomMessage, CustomMessageContent, Message};
use pi_session::{
    AgentMessage, AgentSessionSnapshot, InheritedSessionContext, SessionContext,
    SessionContextBuildOptions, SessionDocument, SessionEntry,
};
use serde_json::{json, Value};

use super::{thread_from_snapshot, SessionTokenUsage};

pub(super) fn separate_inherited_context(
    snapshot: &AgentSessionSnapshot,
    document: &SessionDocument,
) -> AgentSessionSnapshot {
    let mut own = snapshot.clone();
    let Ok(Some(inherited)) = document.inherited_context() else {
        return own;
    };
    // The durable log may be ahead of the subscription snapshot during compaction.
    // Match a complete historical context, never a timestamp or text alone.
    let hidden = inherited_mask(document, &inherited, &own.agent.messages)
        .unwrap_or_else(|| unrecognized_context_mask(document, &inherited, &own.agent.messages));
    for (message, inherited) in own.agent.messages.iter_mut().zip(&hidden) {
        if *inherited {
            *message = Message::custom(CustomMessage {
                custom_type: "pi.desktop.inherited".into(),
                content: CustomMessageContent::Text(String::new()),
                display: false,
                details: None,
                timestamp_ms: 0,
            });
        }
    }
    own
}

fn runtime_messages(messages: Vec<AgentMessage>) -> Vec<Message> {
    SessionContext {
        messages,
        thinking_level: "off".into(),
        model: None,
        active_tool_names: None,
    }
    .runtime_messages()
}

struct InheritedContextProjection {
    messages: Vec<Message>,
    inherited: Vec<bool>,
}

impl InheritedContextProjection {
    fn new(context: &[(AgentMessage, bool)]) -> Self {
        let messages =
            runtime_messages(context.iter().map(|(message, _)| message.clone()).collect());
        let mut source = context
            .iter()
            .filter_map(|(message, inherited)| {
                pi_session::agent_message_to_runtime_message(message)
                    .map(|message| (message, *inherited))
            })
            .peekable();
        let mut previous_inherited = true;
        let inherited = messages
            .iter()
            .map(|message| {
                if source.peek().is_some_and(|(next, _)| next == message) {
                    previous_inherited = source.next().expect("peeked message").1;
                }
                // Runtime repair inserts missing results immediately after the source
                // assistant. They belong to that same source, not a new child action.
                previous_inherited
            })
            .collect();
        Self {
            messages,
            inherited,
        }
    }
}

fn inherited_mask(
    document: &SessionDocument,
    inherited: &InheritedSessionContext,
    snapshot_messages: &[Message],
) -> Option<Vec<bool>> {
    let branch = document.branch().ok()?;
    let entries = branch.into_iter().cloned().collect::<Vec<_>>();
    let options = SessionContextBuildOptions::default();
    let mut context: Vec<(AgentMessage, bool)> = Vec::new();
    let mut matched = None;
    for (index, record) in entries.iter().enumerate() {
        let messages =
            pi_session::session_entry_to_context_messages(record, index, &entries, &options);
        if record.id == inherited.snapshot_entry_id {
            context.extend(messages.into_iter().map(|message| (message, true)));
        } else if let SessionEntry::Compaction(compaction) = &record.entry {
            // Native compaction copies a suffix, preserving complete wire messages.
            // Carry provenance through that suffix; equal text is not an identity.
            let tail = &compaction.retained_tail;
            let retained = context.len().checked_sub(tail.len()).and_then(|start| {
                context[start..]
                    .iter()
                    .map(|(message, _)| message)
                    .eq(tail.iter())
                    .then(|| context[start..].to_vec())
            });
            let retained = retained.unwrap_or_else(|| {
                // Extension-provided tails need not be native suffixes. Only show
                // messages whose own origin can be established from durable data.
                tail.iter()
                    .map(|message| {
                        let is_own = context
                            .iter()
                            .any(|(known, inherited)| !inherited && known == message)
                            && !context
                                .iter()
                                .any(|(known, inherited)| *inherited && known == message);
                        (message.clone(), !is_own)
                    })
                    .collect()
            });
            // A compaction summary includes both sources, not a child-authored turn.
            context = messages
                .into_iter()
                .take(1)
                .map(|message| (message, true))
                .collect();
            context.extend(retained);
        } else {
            context.extend(messages.into_iter().map(|message| (message, false)));
        }
        if record.id == inherited.snapshot_entry_id
            || matches!(record.entry, SessionEntry::Compaction(_))
        {
            let projected = InheritedContextProjection::new(&context);
            if snapshot_messages.starts_with(&projected.messages) {
                matched = Some(projected.inherited);
            }
        }
    }
    matched
}

// Retry repair and extension context rewrites can invalidate every historical
// prefix. Unknown messages must not become child-authored merely because they
// are absent from the seed: show only uniquely identifiable durable child data.
fn unrecognized_context_mask(
    document: &SessionDocument,
    inherited: &InheritedSessionContext,
    snapshot_messages: &[Message],
) -> Vec<bool> {
    let inherited_messages = runtime_messages(inherited.messages.clone());
    let own_messages = document
        .branch()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|record| match &record.entry {
            SessionEntry::Message(entry) => {
                pi_session::agent_message_to_runtime_message(&entry.message)
            }
            SessionEntry::CustomMessage(entry) => Some(entry.to_message(record.timestamp_ms)),
            _ => None,
        })
        .collect::<Vec<_>>();
    snapshot_messages
        .iter()
        .map(|message| inherited_messages.contains(message) || !own_messages.contains(message))
        .collect()
}

pub(super) fn attach_context_origin(
    thread: &mut Value,
    snapshot: &AgentSessionSnapshot,
    document: &SessionDocument,
    parent_thread_id: &str,
) {
    let inherited = document.inherited_context().ok().flatten();
    thread["contextOrigin"] = json!({
        "parentThreadId": document.isolated_parent_session_id().ok().flatten().as_deref().unwrap_or(parent_thread_id),
        "mode": if inherited.is_some() { "fork" } else { "fresh" },
        "parentEntryId": inherited.as_ref().and_then(|seed| seed.parent_entry_id.as_deref()),
        "snapshotEntryId": inherited.as_ref().map(|seed| seed.snapshot_entry_id.as_str()),
    });
    thread["inheritedContext"] = inherited.map_or(Value::Null, |seed| {
        let mut inherited_snapshot = snapshot.clone();
        inherited_snapshot.agent.messages = runtime_messages(seed.messages);
        inherited_snapshot.agent.streaming_message = None;
        inherited_snapshot.agent.is_running = false;
        let projected = thread_from_snapshot(
            &inherited_snapshot,
            &document.header.id,
            &document.header.cwd,
            SessionTokenUsage::default(),
            Value::Null,
            None,
            Default::default(),
        );
        json!({ "turns": projected["turns"] })
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::UserMessage;
    use pi_session::{CustomEntry, SessionHeader, SessionLog};

    #[test]
    fn changed_context_fails_closed_without_hiding_identifiable_child_messages() {
        let directory = tempfile::tempdir().unwrap();
        let log = SessionLog::create(
            directory.path().join("child.jsonl"),
            SessionHeader::new("child", directory.path()),
        )
        .unwrap();
        let parent = Message::User(UserMessage::text("parent history", 1));
        let own = Message::User(UserMessage::text("child task", 3));
        let removed = Message::User(UserMessage::text("removed during repair", 2));
        log.append_session_record(SessionEntry::Custom(CustomEntry {
            custom_type: "pi.isolated_context".into(),
            data: Some(json!({ "parentSessionId": "parent", "messages": [parent, removed] })),
        }))
        .unwrap();
        log.append_session_record(SessionEntry::Message(pi_session::MessageEntry {
            message: AgentMessage::from(own.clone()),
            terminate: false,
        }))
        .unwrap();
        let document = log.load().unwrap();
        for changed_parent in [
            parent.clone(),
            Message::User(UserMessage::text("rewritten parent", 1)),
        ] {
            let snapshot = AgentSessionSnapshot {
                revision: 0,
                agent: pi_agent::AgentStateSnapshot {
                    system_prompt: String::new(),
                    provider_id: pi_core::ProviderId::new("scripted"),
                    model_id: pi_core::ModelId::new("test"),
                    thinking_level: pi_core::ThinkingLevel::Off,
                    active_tools: Vec::new(),
                    messages: vec![changed_parent, own.clone()],
                    is_running: false,
                    streaming_message: None,
                    pending_tool_calls: Default::default(),
                    error_message: None,
                },
                queue: Default::default(),
                compaction: None,
                auto_retry: None,
                bash: None,
                name: None,
            };
            let projected = separate_inherited_context(&snapshot, &document);
            assert!(
                matches!(&projected.agent.messages[0], Message::Custom(custom) if !custom.display)
            );
            assert_eq!(projected.agent.messages[1], own);
            assert_eq!(
                projected.agent.messages.len(),
                snapshot.agent.messages.len()
            );
        }
    }

    #[test]
    fn empty_legacy_fork_snapshot_is_explicit_and_unknown_boundary_stays_null() {
        let directory = tempfile::tempdir().unwrap();
        let log = SessionLog::create(
            directory.path().join("child.jsonl"),
            SessionHeader::new("child", directory.path()),
        )
        .unwrap();
        let record = log
            .append_session_record(SessionEntry::Custom(CustomEntry {
                custom_type: "pi.isolated_context".into(),
                data: Some(json!({ "parentSessionId": "parent", "messages": [] })),
            }))
            .unwrap();
        let stored = super::super::StoredIsolatedSession {
            document: log.load().unwrap(),
            parent_thread_id: "parent".into(),
            agent: "worker".into(),
            nickname: None,
        };
        let thread = super::super::thread_from_stored_isolated(&stored).unwrap();
        assert_eq!(thread["contextOrigin"]["mode"], "fork");
        assert_eq!(thread["contextOrigin"]["parentThreadId"], "parent");
        assert_eq!(thread["contextOrigin"]["snapshotEntryId"], record.id);
        assert!(thread["contextOrigin"]["parentEntryId"].is_null());
        assert_eq!(thread["inheritedContext"]["turns"], json!([]));
    }

    #[test]
    fn reordered_extension_tail_does_not_expose_inherited_items_between_own_messages() {
        let directory = tempfile::tempdir().unwrap();
        let log = SessionLog::create(
            directory.path().join("child.jsonl"),
            SessionHeader::new("child", directory.path()),
        )
        .unwrap();
        let inherited = AgentMessage::from(Message::User(UserMessage::text("same text", 1)));
        let own = AgentMessage::from(Message::User(UserMessage::text("same text", 2)));
        log.append_session_record(SessionEntry::Custom(CustomEntry {
            custom_type: "pi.isolated_context".into(),
            data: Some(json!({ "parentSessionId": "parent", "messages": [inherited] })),
        }))
        .unwrap();
        log.append_session_record(SessionEntry::Message(pi_session::MessageEntry {
            message: own.clone(),
            terminate: false,
        }))
        .unwrap();
        log.append_session_record(SessionEntry::Compaction(pi_session::CompactionEntry {
            summary: "mixed summary".into(),
            retained_tail: vec![own, inherited],
            tokens_before: 10,
            details: None,
            usage: None,
        }))
        .unwrap();
        let stored = super::super::StoredIsolatedSession {
            document: log.load().unwrap(),
            parent_thread_id: "parent".into(),
            agent: "worker".into(),
            nickname: None,
        };
        let thread = super::super::thread_from_stored_isolated(&stored).unwrap();
        let turns = thread["turns"].as_array().unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0]["items"].as_array().unwrap().len(), 1);
        assert_eq!(turns[0]["items"][0]["id"], "user-1-0");
        assert_eq!(
            thread["inheritedContext"]["turns"][0]["items"][0]["id"],
            "user-0-0"
        );
    }
}
