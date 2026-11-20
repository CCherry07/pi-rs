//! Pi Coding summary prompts and file-operation metadata.

use std::collections::HashSet;

use pi_core::Message;
use pi_session::{
    AgentMessage, CompactionPreparation, FileOperations, SessionCompactionPolicy, SessionEntry,
    SessionRecord,
};
use serde_json::{Value, json};

const SUMMARIZATION_PROMPT: &str = r#"The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work.

Use this EXACT format:

## Goal
[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned by user]
- [Or "(none)" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Current work]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [Ordered list of what should happen next]

## Critical Context
- [Any data, examples, or references needed to continue]
- [Or "(none)" if not applicable]

Keep each section concise. Preserve exact file paths, function names, and error messages."#;

const UPDATE_SUMMARIZATION_PROMPT: &str = r#"The messages above are NEW conversation messages to incorporate into the existing summary provided in <previous-summary> tags.

Update the existing structured summary with new information. RULES:
- PRESERVE all existing information from the previous summary
- ADD new progress, decisions, and context from the new messages
- UPDATE the Progress section: move items from "In Progress" to "Done" when completed
- UPDATE "Next Steps" based on what was accomplished
- PRESERVE exact file paths, function names, and error messages
- If something is no longer relevant, you may remove it

Use this EXACT format:

## Goal
[Preserve existing goals, add new ones if the task expanded]

## Constraints & Preferences
- [Preserve existing, add new ones discovered]

## Progress
### Done
- [x] [Include previously done items AND newly completed items]

### In Progress
- [ ] [Current work - update based on progress]

### Blocked
- [Current blockers - remove if resolved]

## Key Decisions
- **[Decision]**: [Brief rationale] (preserve all previous, add new)

## Next Steps
1. [Update based on current state]

## Critical Context
- [Preserve important context, add new if needed]

Keep each section concise. Preserve exact file paths, function names, and error messages."#;

const TURN_PREFIX_SUMMARIZATION_PROMPT: &str = r#"The messages above are earlier context from an ongoing conversation. Later messages are stored separately and do not need to be reconstructed.

Create a concise checkpoint of the user's request and the progress shown above. This checkpoint will be placed before the later messages so the conversation can continue with the necessary context.

## Original Request
[What did the user ask for?]

## Progress So Far
- [Key decisions and work completed in these messages]

## Context Needed to Continue
- [Information from these messages needed to understand the later work]

Only summarize information explicitly present above. Do not infer or recreate later messages."#;

const BRANCH_SUMMARIZATION_PROMPT: &str = "Create a structured summary of this conversation branch for context when returning later.\n\nUse this EXACT format:\n\n## Goal\n[What was the user trying to accomplish in this branch?]\n\n## Constraints & Preferences\n- [Any constraints, preferences, or requirements mentioned]\n- [Or \"(none)\" if none were mentioned]\n\n## Progress\n### Done\n- [x] [Completed tasks/changes]\n\n### In Progress\n- [ ] [Work that was started but not finished]\n\n### Blocked\n- [Issues preventing progress, if any]\n\n## Key Decisions\n- **[Decision]**: [Brief rationale]\n\n## Next Steps\n1. [What should happen next to continue this work]\n\nKeep each section concise. Preserve exact file paths, function names, and error messages.";

/// Coding policy matching Pi's read/write/edit tracking and summary format.
#[derive(Debug, Default)]
pub struct CodingCompactionPolicy;

impl SessionCompactionPolicy for CodingCompactionPolicy {
    fn summary_prompt(&self, updating: bool) -> &str {
        if updating {
            UPDATE_SUMMARIZATION_PROMPT
        } else {
            SUMMARIZATION_PROMPT
        }
    }

    fn turn_prefix_prompt(&self) -> &str {
        TURN_PREFIX_SUMMARIZATION_PROMPT
    }

    fn branch_prompt(&self) -> &str {
        BRANCH_SUMMARIZATION_PROMPT
    }

    fn prepare(&self, path_entries: &[SessionRecord], preparation: &mut CompactionPreparation) {
        let mut file_ops = FileOperations::default();
        if let Some(previous) = path_entries
            .iter()
            .rev()
            .find_map(|record| match &record.entry {
                SessionEntry::Compaction(previous) => Some(previous),
                _ => None,
            })
            && let Some(details) = &previous.details
        {
            extend_string_set(&mut file_ops.read, details.get("readFiles"));
            extend_string_set(&mut file_ops.edited, details.get("modifiedFiles"));
        }
        for message in preparation
            .messages_to_summarize
            .iter()
            .chain(&preparation.turn_prefix_messages)
        {
            extract_file_operations(message, &mut file_ops);
        }
        preparation.file_ops = file_ops;
    }

    fn prepare_branch(
        &self,
        path_entries: &[SessionRecord],
        preparation: &mut CompactionPreparation,
    ) {
        let mut file_ops = FileOperations::default();
        for entry in path_entries {
            if let SessionEntry::BranchSummary(summary) = &entry.entry
                && let Some(details) = &summary.details
            {
                extend_string_set(&mut file_ops.read, details.get("readFiles"));
                extend_string_set(&mut file_ops.edited, details.get("modifiedFiles"));
            }
        }
        for message in &preparation.messages_to_summarize {
            extract_file_operations(message, &mut file_ops);
        }
        preparation.file_ops = file_ops;
    }

    fn finalize(&self, preparation: &CompactionPreparation, summary: &mut String) -> Option<Value> {
        let (read_files, modified_files) = compute_file_lists(&preparation.file_ops);
        summary.push_str(&format_file_operations(&read_files, &modified_files));
        Some(json!({ "readFiles": read_files, "modifiedFiles": modified_files }))
    }
}

fn extract_file_operations(message: &AgentMessage, file_ops: &mut FileOperations) {
    if message.role() == "toolResult" {
        // AgentMessage preserves unknown wire extensions, including the calls
        // executed within a codemode script's result.
        if let Ok(value) = serde_json::to_value(message)
            && let Some(calls) = value
                .pointer("/nestedCalls/calls")
                .and_then(Value::as_array)
        {
            for call in calls {
                if let (Some(name), Some(arguments)) = (
                    call.get("name").and_then(Value::as_str),
                    call.get("arguments"),
                ) {
                    add_file_operation(name, arguments, file_ops);
                }
            }
        }
    } else if let Some(Message::Assistant(message)) = message.as_standard() {
        for call in message.tool_calls() {
            add_file_operation(&call.name, &call.arguments, file_ops);
        }
    }
}

fn add_file_operation(name: &str, arguments: &Value, file_ops: &mut FileOperations) {
    let Some(path) = arguments
        .get("path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
    else {
        return;
    };
    match name {
        "read" => {
            file_ops.read.insert(path.to_string());
        }
        "write" => {
            file_ops.written.insert(path.to_string());
        }
        "edit" => {
            file_ops.edited.insert(path.to_string());
        }
        _ => {}
    }
}

fn extend_string_set(target: &mut HashSet<String>, value: Option<&Value>) {
    if let Some(Value::Array(values)) = value {
        target.extend(values.iter().filter_map(Value::as_str).map(str::to_string));
    }
}

fn compute_file_lists(file_ops: &FileOperations) -> (Vec<String>, Vec<String>) {
    let modified = file_ops
        .written
        .union(&file_ops.edited)
        .cloned()
        .collect::<HashSet<_>>();
    let mut read_files = file_ops
        .read
        .difference(&modified)
        .cloned()
        .collect::<Vec<_>>();
    let mut modified_files = modified.into_iter().collect::<Vec<_>>();
    read_files.sort();
    modified_files.sort();
    (read_files, modified_files)
}

fn format_file_operations(read_files: &[String], modified_files: &[String]) -> String {
    let mut sections = Vec::new();
    if !read_files.is_empty() {
        sections.push(format!(
            "<read-files>\n{}\n</read-files>",
            read_files.join("\n")
        ));
    }
    if !modified_files.is_empty() {
        sections.push(format!(
            "<modified-files>\n{}\n</modified-files>",
            modified_files.join("\n")
        ));
    }
    if sections.is_empty() {
        String::new()
    } else {
        format!("\n\n{}", sections.join("\n\n"))
    }
}

#[cfg(test)]
mod tests {
    use pi_core::{
        AssistantMessage, ContentBlock, ModelId, ProviderId, StopReason, ToolCall, Usage,
    };
    use pi_session::{CompactionEntry, CompactionSettings};

    use super::*;

    fn calls(tools: &[(&str, &str)]) -> AgentMessage {
        Message::assistant(AssistantMessage {
            duration_ms: None,
            thinking_level: None,
            content: tools
                .iter()
                .enumerate()
                .map(|(index, (name, path))| {
                    ContentBlock::ToolCall(ToolCall::new(
                        index.to_string(),
                        *name,
                        json!({"path": path}),
                    ))
                })
                .collect(),
            api: "scripted".to_string(),
            provider: ProviderId::new("scripted"),
            model: ModelId::new("test"),
            provider_thinking_level: None,
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: StopReason::ToolUse,
            error_message: None,
            deferred: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp_ms: 0,
        })
        .into()
    }

    #[test]
    fn coding_checkpoint_prompts_match_current_legacy_text() {
        // Literal oracles from compaction.ts and branch-summarization.ts in
        // legacy/pi/packages/coding-agent/src/core/compaction (05fd3fbda).
        assert_eq!(
            CodingCompactionPolicy.turn_prefix_prompt(),
            r#"The messages above are earlier context from an ongoing conversation. Later messages are stored separately and do not need to be reconstructed.

Create a concise checkpoint of the user's request and the progress shown above. This checkpoint will be placed before the later messages so the conversation can continue with the necessary context.

## Original Request
[What did the user ask for?]

## Progress So Far
- [Key decisions and work completed in these messages]

## Context Needed to Continue
- [Information from these messages needed to understand the later work]

Only summarize information explicitly present above. Do not infer or recreate later messages."#
        );
        assert_eq!(
            CodingCompactionPolicy.branch_prompt(),
            r#"Create a structured summary of this conversation branch for context when returning later.

Use this EXACT format:

## Goal
[What was the user trying to accomplish in this branch?]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned]
- [Or "(none)" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Work that was started but not finished]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [What should happen next to continue this work]

Keep each section concise. Preserve exact file paths, function names, and error messages."#
        );
    }

    #[test]
    fn file_tracking_carries_previous_checkpoint_and_excludes_retained_tail() {
        let previous = SessionRecord {
            id: "checkpoint".to_string(),
            seq: 0,
            parent_id: None,
            timestamp_ms: 0,
            entry: SessionEntry::Compaction(CompactionEntry {
                from_hook: None,
                system_message: None,
                retained_entry_ids: Vec::new(),
                summary: "previous".to_string(),
                retained_tail: Vec::new(),
                tokens_before: 1_000,
                details: Some(json!({
                    "readFiles": ["README.md", "src/lib.rs"],
                    "modifiedFiles": ["Cargo.toml"],
                })),
                usage: None,
            }),
        };
        let mut preparation = CompactionPreparation {
            messages_to_summarize: vec![calls(&[
                ("edit", "src/lib.rs"),
                ("read", "docs/guide.md"),
            ])],
            turn_prefix_messages: vec![calls(&[("write", "src/new.rs"), ("lookup", "orders/42")])],
            retained_tail: vec![calls(&[("edit", "retained.rs")])],
            is_split_turn: true,
            tokens_before: 2_000,
            previous_summary: Some("previous".to_string()),
            file_ops: FileOperations::default(),
            settings: CompactionSettings::default(),
        };

        CodingCompactionPolicy.prepare(&[previous], &mut preparation);
        let mut summary = "summary".to_string();
        let details = CodingCompactionPolicy
            .finalize(&preparation, &mut summary)
            .unwrap();

        assert_eq!(
            details,
            json!({
                "readFiles": ["README.md", "docs/guide.md"],
                "modifiedFiles": ["Cargo.toml", "src/lib.rs", "src/new.rs"],
            })
        );
        assert_eq!(
            summary,
            "summary\n\n<read-files>\nREADME.md\ndocs/guide.md\n</read-files>\n\n<modified-files>\nCargo.toml\nsrc/lib.rs\nsrc/new.rs\n</modified-files>"
        );
        assert!(
            CodingCompactionPolicy
                .summary_prompt(false)
                .contains("Preserve exact file paths, function names")
        );
        assert!(
            CodingCompactionPolicy
                .summary_prompt(true)
                .contains("PRESERVE exact file paths, function names")
        );
    }

    #[test]
    fn coding_compaction_tracks_nested_calls_and_ignores_empty_paths() {
        let mut preparation = CompactionPreparation {
            messages_to_summarize: vec![
                calls(&[("read", ""), ("write", "direct.rs")]),
                AgentMessage::custom(json!({
                    "role":"toolResult", "toolCallId":"script", "toolName":"codemode",
                    "content":[{"type":"text", "text":"done"}], "isError":false, "timestamp":0,
                    "nestedCalls":{"calls":[
                        {"name":"read", "arguments":{"path":"nested.md"}},
                        {"name":"read", "arguments":{"path":"changed.rs"}},
                        {"name":"edit", "arguments":{"path":"changed.rs"}},
                        {"name":"write", "arguments":{"path":"created.rs"}},
                        {"name":"read", "arguments":{"path":""}},
                        {"name":"read", "arguments":{"path":42}},
                        {"name":"lookup", "arguments":{"path":"not-a-file"}}
                    ]}
                }))
                .unwrap(),
            ],
            turn_prefix_messages: Vec::new(),
            retained_tail: Vec::new(),
            is_split_turn: false,
            tokens_before: 100,
            previous_summary: None,
            file_ops: FileOperations::default(),
            settings: CompactionSettings::default(),
        };
        CodingCompactionPolicy.prepare(&[], &mut preparation);
        let mut summary = "checkpoint".to_string();
        assert_eq!(
            CodingCompactionPolicy.finalize(&preparation, &mut summary),
            Some(json!({
                "readFiles":["nested.md"], "modifiedFiles":["changed.rs", "created.rs", "direct.rs"]
            }))
        );
        assert_eq!(
            summary,
            "checkpoint\n\n<read-files>\nnested.md\n</read-files>\n\n<modified-files>\nchanged.rs\ncreated.rs\ndirect.rs\n</modified-files>"
        );
    }

    #[tokio::test]
    async fn coding_branch_summary_uses_file_tracking_and_the_legacy_wrapper() {
        use pi_agent::AgentOptions;
        use pi_core::WorkspaceSpec;
        use pi_runtime::PiRuntime;
        use pi_session::{AgentSession, AgentSessionOptions};
        use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};

        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("context.txt"), "domain context").unwrap();
        let plugin = ScriptedProviderPlugin::scripted([
            ScriptedTurn::Text("root answer".to_string()),
            ScriptedTurn::ToolCalls(vec![ToolCall::new(
                "read-context",
                "read",
                json!({"path":"context.txt"}),
            )]),
            ScriptedTurn::Text("branch answer".to_string()),
            ScriptedTurn::Text("branch checkpoint".to_string()),
        ]);
        let provider = plugin.provider();
        let runtime = PiRuntime::builder()
            .workspace(WorkspaceSpec::from_cwd(directory.path()))
            .provider_plugin(plugin)
            .plugin(pi_plugin_read::ReadPlugin)
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("scripted"),
                model_id: ModelId::new("test"),
                ..AgentOptions::default()
            })
            .build()
            .unwrap();
        let session = AgentSession::create(
            runtime,
            directory.path().join("session.jsonl"),
            AgentSessionOptions::default()
                .compaction_policy(std::sync::Arc::new(CodingCompactionPolicy)),
        )
        .await
        .unwrap();
        session.prompt("root").await.unwrap();
        let target = session.history().leaf_id().unwrap();
        session.prompt("read context.txt").await.unwrap();
        session
            .summarize_branch_and_checkout(&target, Some("Keep paths".to_string()), false, None)
            .await
            .unwrap();
        let document = session.history().load().unwrap();
        let summary = document
            .entries
            .iter()
            .find_map(|record| match &record.entry {
                SessionEntry::BranchSummary(summary) => Some(summary),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            summary.details,
            Some(json!({"readFiles":["context.txt"], "modifiedFiles":[]}))
        );
        assert_eq!(
            summary.summary,
            "The user explored a different conversation branch before returning here.\nSummary of that exploration:\n\nbranch checkpoint\n\n<read-files>\ncontext.txt\n</read-files>"
        );
        let requests = provider.requests();
        let request = requests.last().unwrap();
        assert_eq!(request.max_output_tokens, None);
        assert!(request.tools.is_empty());
        assert!(matches!(&request.messages[..], [Message::User(user)]
            if matches!(&user.content[..], [ContentBlock::Text(text)]
                if text.text.starts_with("<conversation>\n[User]: read context.txt\n\n[Assistant tool calls]: read(path=\"context.txt\")\n\n[Assistant]: branch answer\n</conversation>\n\n")
                    && text.text.ends_with("Additional focus: Keep paths"))));
        session.shutdown().await;
    }

    #[tokio::test]
    async fn coding_session_configuration_preserves_file_metadata_in_compaction() {
        use pi_agent::AgentOptions;
        use pi_core::WorkspaceSpec;
        use pi_runtime::PiRuntime;
        use pi_session::{AgentSession, InitialModelRequest};
        use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};

        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("context.txt"), "domain context").unwrap();
        let provider_plugin = ScriptedProviderPlugin::scripted([
            ScriptedTurn::ToolCalls(vec![ToolCall::new(
                "read-context",
                "read",
                json!({"path":"context.txt"}),
            )]),
            ScriptedTurn::Text("Read the context and completed the work.".to_string()),
            ScriptedTurn::Text("Summary of the completed work.".to_string()),
        ]);
        let provider = provider_plugin.provider();
        let runtime = PiRuntime::builder()
            .workspace(WorkspaceSpec::from_cwd(directory.path()))
            .provider_plugin(provider_plugin)
            .plugin(pi_plugin_read::ReadPlugin)
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("scripted"),
                model_id: ModelId::new("test"),
                ..AgentOptions::default()
            })
            .build()
            .unwrap();
        let settings = pi_settings::SettingsValues {
            compaction: pi_settings::CompactionSettings {
                keep_recent_tokens: 1,
                ..pi_settings::CompactionSettings::default()
            },
            ..pi_settings::SettingsValues::default()
        };
        let options =
            crate::configuration::session_options(&settings, InitialModelRequest::default());
        let session =
            AgentSession::create(runtime, directory.path().join("session.jsonl"), options)
                .await
                .unwrap();
        session
            .prompt("Read context.txt before completing the task.")
            .await
            .unwrap();

        let compacted = session.compact(None).await.unwrap();

        assert_eq!(
            compacted.details,
            Some(json!({ "readFiles": ["context.txt"], "modifiedFiles": [] }))
        );
        assert!(
            compacted
                .summary
                .ends_with("<read-files>\ncontext.txt\n</read-files>")
        );
        assert!(provider.requests().last().unwrap().tools.is_empty());
        session.shutdown().await;
    }
}
