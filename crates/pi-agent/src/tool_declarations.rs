//! Declare executable loadout changes in the real transcript before publishing it.
use pi_core::{Message, SystemMessage, ToolDeclaration, ToolReference, ToolSpec};

pub(crate) fn declare_tool_changes(
    committed: &[Message],
    pending: &mut Vec<Message>,
    tools: &[ToolSpec],
    timestamp_ms: i64,
) {
    // The final pending system message is intent, not a second committed baseline.
    let system_index = pending
        .iter()
        .rposition(|message| matches!(message, Message::System(_)));
    let intent = system_index.map(|index| {
        let Message::System(message) = &mut pending[index] else {
            unreachable!()
        };
        (message.tools_added.take(), message.tools_removed.take())
    });
    let previous = SystemMessage::current_tools(committed.iter().chain(pending.iter()));
    let desired: Vec<_> = tools.iter().map(ToolDeclaration::from).collect();
    // Pi compares serialized declarations, not structural schema equality.
    // Preserve object insertion order and last-definition-by-name semantics.
    let by_name = |tools: &[ToolDeclaration]| {
        tools
            .iter()
            .map(|tool| {
                (
                    tool.name.clone(),
                    serde_json::to_vec(tool).expect("tool declaration is JSON"),
                )
            })
            .collect::<std::collections::HashMap<_, _>>()
    };
    let previous_by_name = by_name(&previous);
    let desired_by_name = by_name(&desired);
    let changed = |tool: &&ToolDeclaration| {
        previous_by_name.get(&tool.name) != desired_by_name.get(&tool.name)
    };
    let added: Vec<_> = desired.iter().filter(changed).cloned().collect();
    let removed: Vec<_> = previous
        .iter()
        .filter(changed)
        .map(|tool| ToolReference {
            name: tool.name.clone(),
        })
        .collect();
    if let Some(index) = system_index {
        let Message::System(message) = &mut pending[index] else {
            unreachable!()
        };
        let (original_added, original_removed) = intent.expect("pending system intent");
        if added.is_empty()
            && removed.is_empty()
            && original_added.as_ref().is_none_or(Vec::is_empty)
            && original_removed.as_ref().is_none_or(Vec::is_empty)
        {
            message.tools_added = original_added;
            message.tools_removed = original_removed;
        } else {
            message.tools_added = (!added.is_empty()).then_some(added);
            message.tools_removed = (!removed.is_empty()).then_some(removed);
        }
    } else if !added.is_empty() || !removed.is_empty() {
        let mut message = SystemMessage::text("", timestamp_ms);
        message.tools_added = (!added.is_empty()).then_some(added);
        message.tools_removed = (!removed.is_empty()).then_some(removed);
        let index = pending
            .iter()
            .position(|message| !matches!(message, Message::System(_)))
            .unwrap_or(pending.len());
        pending.insert(index, Message::System(message));
    }
}

/// A request-only projection over the complete transcript; never rewrite the
/// committed declaration state just because this request hides a definition.
pub(crate) fn hide_request_declarations(messages: &mut [Message], hidden: &[String]) {
    if hidden.is_empty() {
        return;
    }
    for message in messages {
        let Message::System(message) = message else {
            continue;
        };
        if let Some(added) = &mut message.tools_added {
            added.retain(|tool| !hidden.contains(&tool.name));
            if added.is_empty() {
                message.tools_added = None;
            }
        }
        if let Some(removed) = &mut message.tools_removed {
            removed.retain(|tool| !hidden.contains(&tool.name));
            if removed.is_empty() {
                message.tools_removed = None;
            }
        }
    }
}
