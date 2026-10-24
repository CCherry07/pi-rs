//! Semantic transcript projection for plugin render callbacks.
use super::super::{App, ShellState, ToolState, TranscriptItem};
use pi_plugin::UiTranscriptItem;
use serde_json::json;
use std::hash::{Hash, Hasher};

pub(in crate::tui) fn snapshot(app: &App) -> Vec<UiTranscriptItem> {
    app.transcript.iter().enumerate().filter_map(|(index, item)| {
        let payload = match item {
            TranscriptItem::User(text) => json!({ "kind": "user", "text": text, "streaming": false }),
            TranscriptItem::Assistant { text, thinking, streaming, error } => json!({
                "kind": "assistant", "text": text, "thinking": thinking, "streaming": streaming, "error": error,
            }),
            TranscriptItem::Custom { entry, message, .. } => json!({ "kind": if *message { "customMessage" } else { "customEntry" }, "entry": entry }),
            TranscriptItem::Tool { id, name, input, render_result, state, .. } => json!({
                "kind": "tool", "toolCallId": id.as_str(), "name": name,
                "args": input.as_deref().and_then(|input| serde_json::from_str::<serde_json::Value>(input).ok()).unwrap_or(json!({})),
                "result": render_result, "executionStarted": !matches!(state, ToolState::Pending),
                "argsComplete": !matches!(state, ToolState::Pending) || app.streaming_assistant.is_none(),
                "isPartial": matches!(state, ToolState::Pending | ToolState::Running),
                "isError": matches!(state, ToolState::Failed(_)), "cwd": app.cwd,
            }),
            TranscriptItem::Shell { command, output, state, .. } => json!({
                "kind": "tool", "toolCallId": format!("shell-{index}"), "name": "bash", "args": { "command": command },
                "result": { "content": [{ "type": "text", "text": output }] },
                "executionStarted": true, "argsComplete": true, "isPartial": matches!(state, ShellState::Running),
                "isError": matches!(state, ShellState::Failed(_) | ShellState::Finished { exit_code: Some(1..), .. }), "cwd": app.cwd,
            }),
            TranscriptItem::Notice(_) => return None,
        };
        let mut hash = std::hash::DefaultHasher::new();
        payload.to_string().hash(&mut hash);
        Some(UiTranscriptItem { index, fingerprint: format!("{:016x}", hash.finish()), payload })
    }).collect()
}
