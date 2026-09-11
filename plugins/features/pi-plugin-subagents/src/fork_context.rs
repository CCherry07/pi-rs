use std::collections::HashSet;

use pi_core::{ContentBlock, Message};

/// Remove parent-only collaboration calls from a forked child prefix while
/// retaining the child's own collaboration history after its launch marker.
pub(crate) fn project_inherited_messages(messages: Vec<Message>, agent_id: &str) -> Vec<Message> {
    let Some(boundary) = messages.iter().position(|message| {
        crate::marker_from_messages(std::slice::from_ref(message)) == Some(agent_id)
    }) else {
        return messages;
    };
    let excluded = messages[..boundary]
        .iter()
        .flat_map(|message| match message {
            Message::Assistant(message) => message
                .tool_calls()
                .into_iter()
                .filter(|call| is_collaboration_tool(&call.name))
                .map(|call| call.id.clone())
                .collect(),
            _ => Vec::new(),
        })
        .collect::<HashSet<_>>();
    messages
        .into_iter()
        .enumerate()
        .filter_map(|(index, message)| {
            if index >= boundary {
                return Some(strip_marker(message));
            }
            filter_parent_message(message, &excluded)
        })
        .collect()
}

fn filter_parent_message(
    mut message: Message,
    excluded: &HashSet<pi_core::ToolCallId>,
) -> Option<Message> {
    match &mut message {
        Message::Assistant(message) => {
            let message = std::sync::Arc::make_mut(message);
            let anthropic = message.provider.as_str().eq_ignore_ascii_case("anthropic")
                || message.api.eq_ignore_ascii_case("anthropic-messages")
                || message
                    .model
                    .as_str()
                    .to_ascii_lowercase()
                    .starts_with("anthropic/");
            message.content.retain(|block| match block {
                ContentBlock::ToolCall(call) => !is_collaboration_tool(&call.name),
                ContentBlock::Thinking(thinking) if anthropic => {
                    thinking.redacted != Some(true)
                        && thinking
                            .thinking_signature
                            .as_ref()
                            .is_none_or(String::is_empty)
                }
                _ => true,
            });
            (!message.content.is_empty()).then_some(())?;
        }
        Message::ToolResult(message)
            if is_collaboration_tool(&message.tool_name)
                || excluded.contains(&message.tool_call_id) =>
        {
            return None;
        }
        Message::Custom(message)
            if matches!(
                message.custom_type.as_str(),
                "agent_message" | "agent_settled"
            ) =>
        {
            return None;
        }
        _ => {}
    }
    Some(strip_marker(message))
}

fn strip_marker(mut message: Message) -> Message {
    if let Message::User(user) = &mut message {
        for block in &mut user.content {
            if let ContentBlock::Text(text) = block
                && crate::runtime::run_marker(&text.text).is_some()
            {
                text.text = text
                    .text
                    .split_once('\n')
                    .map_or("", |(_, task)| task)
                    .to_string();
            }
        }
    }
    message
}

fn is_collaboration_tool(name: &str) -> bool {
    matches!(
        name,
        "spawn_agent"
            | "send_message"
            | "followup_task"
            | "wait_agent"
            | "interrupt_agent"
            | "list_agents"
    )
}

#[cfg(test)]
mod tests {
    use pi_core::{AssistantMessage, TextContent, ToolCall, ToolResultMessage, UserMessage};
    use serde_json::json;

    use super::*;

    fn assistant_call(id: &str, name: &str) -> Message {
        Message::assistant(AssistantMessage {
            content: vec![
                ContentBlock::Text(TextContent::new("keep parent reasoning")),
                ContentBlock::ToolCall(ToolCall::new(id, name, json!({}))),
            ],
            api: "test".into(),
            provider: "test".into(),
            model: "test".into(),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: Default::default(),
            stop_reason: pi_core::StopReason::ToolUse,
            error_message: None,
            deferred: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp_ms: 1,
        })
    }

    #[test]
    fn fork_filters_parent_collaboration_and_keeps_child_collaboration() {
        let parent_call = assistant_call("parent-spawn", "spawn_agent");
        let parent_result = Message::ToolResult(
            ToolResultMessage {
                tool_call_id: "parent-spawn".into(),
                tool_name: "spawn_agent".into(),
                content: vec![ContentBlock::Text(TextContent::new("started"))],
                details: None,
                is_error: false,
                usage: None,
                added_tool_names: None,
                timestamp_ms: 2,
            }
            .into(),
        );
        let task = Message::User(UserMessage::text("<!-- pi-rs-agent:child -->\ninspect", 3));
        let child_call = assistant_call("child-list", "list_agents");
        let projected = project_inherited_messages(
            vec![parent_call, parent_result, task, child_call.clone()],
            "child",
        );
        assert_eq!(projected.len(), 3);
        assert!(
            matches!(&projected[0], Message::Assistant(message) if message.tool_calls().is_empty())
        );
        assert!(
            matches!(&projected[1], Message::User(user) if matches!(&user.content[0], ContentBlock::Text(text) if text.text == "inspect"))
        );
        assert_eq!(projected[2], child_call);
    }
}
