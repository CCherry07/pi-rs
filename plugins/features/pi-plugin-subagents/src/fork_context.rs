use std::collections::HashSet;

use pi_core::{ContentBlock, Message};

/// Parent-only orchestration is policy of this plugin, not the session runtime.
/// Only the inherited prefix is filtered: a nested child's own delegation must
/// retain both its call and result in subsequent provider requests.
pub(crate) fn project_inherited_messages(messages: Vec<Message>, run_id: &str) -> Vec<Message> {
    let Some(boundary) = messages.iter().position(|message| {
        crate::marker_from_messages(std::slice::from_ref(message)) == Some(run_id)
    }) else {
        return messages;
    };
    let excluded = messages[..boundary]
        .iter()
        .flat_map(|message| match message {
            Message::Assistant(message) => message
                .tool_calls()
                .into_iter()
                .filter(|call| is_coordination_tool(&call.name))
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
                return Some(message);
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
            message.content.retain(
                |block| !matches!(block, ContentBlock::ToolCall(call) if is_coordination_tool(&call.name)),
            );
            if message.content.is_empty() {
                return None;
            }
        }
        Message::ToolResult(message)
            if is_coordination_tool(&message.tool_name)
                || excluded.contains(&message.tool_call_id) =>
        {
            return None;
        }
        Message::Custom(message)
            if matches!(
                message.custom_type.as_str(),
                "subagent-orchestration-instructions"
                    | "subagent_supervisor_request"
                    | "subagent_supervisor_reply"
                    | "subagent-slash-result"
                    | "subagent-slash-text-result"
                    | "subagent-notify"
                    | "subagent_control_notice"
                    | "subagent-control"
                    | "subagent-control-notice"
                    | "subagent_watchdog_warning"
            ) =>
        {
            return None;
        }
        Message::User(message) => {
            for block in &mut message.content {
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
        _ => {}
    }
    Some(message)
}

fn is_coordination_tool(name: &str) -> bool {
    matches!(
        name,
        "subagent" | "contact_supervisor" | "subagent_supervisor" | "bg_wait"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::{AssistantMessage, TextContent, ToolCall, ToolResultMessage, UserMessage};
    use serde_json::json;

    fn assistant() -> Message {
        Message::assistant(AssistantMessage {
            content: vec![
                ContentBlock::Text(TextContent::new("parent explanation")),
                ContentBlock::ToolCall(ToolCall::new("old-child", "subagent", json!({}))),
                ContentBlock::ToolCall(ToolCall::new("read-file", "read", json!({}))),
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

    fn result(id: &str, name: &str) -> Message {
        Message::ToolResult(
            ToolResultMessage {
                tool_call_id: id.into(),
                tool_name: name.into(),
                content: vec![ContentBlock::Text(TextContent::new("result"))],
                details: None,
                is_error: false,
                usage: None,
                added_tool_names: None,
                timestamp_ms: 2,
            }
            .into(),
        )
    }

    #[test]
    fn filters_only_inherited_orchestration_and_preserves_other_tool_pairs() {
        let child_task = Message::User(UserMessage::text(
            "<!-- pi-rs-subagent-run:current -->\nchild task",
            3,
        ));
        let parent_task = Message::User(UserMessage::text(
            "<!-- pi-rs-subagent-run:parent -->\nparent task",
            0,
        ));
        let own = assistant();
        let input = vec![
            parent_task,
            assistant(),
            result("old-child", "subagent"),
            result("read-file", "read"),
            child_task.clone(),
            own.clone(),
        ];
        let parent = input.clone();
        let projected = project_inherited_messages(input, "current");
        assert_eq!(projected.len(), 5);
        assert!(
            matches!(&projected[0], Message::User(user) if matches!(&user.content[0], ContentBlock::Text(text) if text.text == "parent task"))
        );
        assert!(
            matches!(&projected[1], Message::Assistant(message) if message.tool_calls().len() == 1 && message.tool_calls()[0].name == "read")
        );
        assert!(
            matches!(&projected[2], Message::ToolResult(message) if message.tool_call_id.as_str() == "read-file")
        );
        assert_eq!(projected[3], child_task);
        assert_eq!(projected[4], own);
        assert!(
            matches!(&parent[1], Message::Assistant(message) if message.tool_calls().len() == 2)
        );
    }

    #[test]
    fn fork_removes_parent_supervisor_pairs_but_keeps_child_pairs() {
        for name in ["contact_supervisor", "subagent_supervisor", "bg_wait"] {
            let mut call = assistant();
            if let Message::Assistant(message) = &mut call {
                std::sync::Arc::make_mut(message).content = vec![ContentBlock::ToolCall(
                    ToolCall::new("coord", name, json!({})),
                )];
            }
            let reply = result("coord", name);
            let task = Message::User(UserMessage::text(
                "<!-- pi-rs-subagent-run:current -->\nchild task",
                3,
            ));
            let projected = project_inherited_messages(
                vec![
                    call.clone(),
                    reply.clone(),
                    task.clone(),
                    call.clone(),
                    reply.clone(),
                ],
                "current",
            );
            assert_eq!(projected, vec![task, call, reply]);
        }
    }
}
