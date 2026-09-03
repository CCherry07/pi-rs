use async_trait::async_trait;
use pi_core::{
    Tool, ToolCallId, ToolContext, ToolError, ToolExecutionMode, ToolResult, ToolSpec,
    ToolUpdateSink,
};
use serde_json::{Value, json};

use super::output::{
    MAX_RENDERED_TEXT_BYTES, bounded_lines, execution, query_limit, required_string,
};
use crate::runtime::{LocalMemoryRuntime, truncate_utf8};
use crate::{
    MEMORY_EVENT_TYPE, MemoryEvidence, MemoryKind, MemoryMutation, MemoryOrigin, MemoryRecord,
    MemoryScope, RecallQuery,
};

pub(super) struct MemoryTool {
    runtime: LocalMemoryRuntime,
}

impl MemoryTool {
    pub(super) fn new(runtime: LocalMemoryRuntime) -> Self {
        Self { runtime }
    }

    async fn write_record(
        &self,
        context: &ToolContext,
        tool_call_id: &ToolCallId,
        input: &Value,
        supersedes: Option<&str>,
    ) -> Result<ToolResult, ToolError> {
        let text = required_string(input, "text")?.trim();
        let evidence = required_string(input, "evidence")?.trim();
        let snapshot = context.session.snapshot()?;
        let scope = write_scope(
            input.get("scope").and_then(Value::as_str),
            &self.runtime,
            snapshot.id(),
        )?;
        let record = MemoryRecord {
            id: uuid::Uuid::now_v7().to_string(),
            scope,
            kind: memory_kind(input.get("kind").and_then(Value::as_str))?,
            text: text.to_string(),
            origin: MemoryOrigin {
                session_id: snapshot.id().to_string(),
                entry_id: snapshot.leaf_id().map(str::to_string),
                tool_call_id: Some(tool_call_id.to_string()),
            },
            evidence: MemoryEvidence {
                note: evidence.to_string(),
            },
            recorded_at_ms: now_ms(),
            supersedes: supersedes.map(str::to_string),
        };
        let mutation = MemoryMutation::Remember {
            mutation_id: uuid::Uuid::now_v7().to_string(),
            record: record.clone(),
        };
        persist_then_apply(context, &self.runtime, &mutation)
            .await
            .map(|indexed| {
                let verb = if supersedes.is_some() {
                    "Corrected"
                } else {
                    "Remembered"
                };
                let message = if indexed {
                    format!("{verb} memory {}.", record.id)
                } else {
                    format!(
                        "{verb} memory {} in the session journal; local index update is pending reconciliation.",
                        record.id
                    )
                };
                let mut result = ToolResult::text(message);
                result.details = Some(json!({
                    "record": record,
                    "mutationId": mutation.id(),
                    "indexed": indexed
                }));
                result
            })
    }

    async fn forget(
        &self,
        context: &ToolContext,
        tool_call_id: &ToolCallId,
        input: &Value,
    ) -> Result<ToolResult, ToolError> {
        let target_id = required_string(input, "targetId")?.trim();
        let reason = input
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("No longer applicable")
            .trim();
        let snapshot = context.session.snapshot()?;
        let mutation = MemoryMutation::Forget {
            mutation_id: uuid::Uuid::now_v7().to_string(),
            target_id: target_id.to_string(),
            reason: reason.to_string(),
            origin: MemoryOrigin {
                session_id: snapshot.id().to_string(),
                entry_id: snapshot.leaf_id().map(str::to_string),
                tool_call_id: Some(tool_call_id.to_string()),
            },
            recorded_at_ms: now_ms(),
        };
        let indexed = persist_then_apply(context, &self.runtime, &mutation).await?;
        let message = if indexed {
            format!("Forgot memory {target_id}.")
        } else {
            format!(
                "Recorded forgetting memory {target_id} in the session journal; local index update is pending reconciliation."
            )
        };
        let mut result = ToolResult::text(message);
        result.details = Some(json!({
            "targetId": target_id,
            "mutationId": mutation.id(),
            "indexed": indexed
        }));
        Ok(result)
    }

    async fn query(
        &self,
        context: &ToolContext,
        input: &Value,
        require_query: bool,
    ) -> Result<ToolResult, ToolError> {
        let snapshot = context.session.snapshot()?;
        let text = input.get("text").and_then(Value::as_str).unwrap_or("");
        if require_query && text.trim().is_empty() {
            return Err(ToolError::InvalidArguments(
                "search requires non-empty text".to_string(),
            ));
        }
        let scopes = read_scopes(
            input.get("scope").and_then(Value::as_str),
            &self.runtime,
            snapshot.id(),
        )?;
        let result = self
            .runtime
            .recall(RecallQuery {
                text: text.to_string(),
                scopes,
                limit: query_limit(input)?,
            })
            .await
            .map_err(execution)?;
        if result.hits.is_empty() {
            return Ok(ToolResult::text("No matching memories."));
        }
        let lines = result
            .hits
            .iter()
            .map(|hit| {
                format!(
                    "- [{}] {} ({})\n  {}",
                    hit.record.id,
                    hit.record.kind.as_str(),
                    hit.record.scope.key(),
                    truncate_utf8(&hit.record.text, MAX_RENDERED_TEXT_BYTES)
                )
            })
            .collect::<Vec<_>>();
        let text = bounded_lines(&lines);
        let mut tool_result = ToolResult::text(text);
        tool_result.details = Some(json!({
            "matches": result.hits.iter().map(|hit| json!({
                "id": hit.record.id,
                "scope": hit.record.scope.key(),
                "kind": hit.record.kind.as_str(),
                "score": hit.score
            })).collect::<Vec<_>>()
        }));
        Ok(tool_result)
    }
}

#[async_trait]
impl Tool for MemoryTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "memory".to_string(),
            label: "Memory".to_string(),
            description: "Remember, correct, forget, list, or search durable user-approved memory. Never store passwords, API keys, tokens, private keys, or other secrets.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["remember", "correct", "forget", "list", "search"]
                    },
                    "text": { "type": "string", "description": "Memory text for remember/correct, or query text for search" },
                    "targetId": { "type": "string", "description": "Record id to correct or forget" },
                    "scope": { "type": "string", "enum": ["user", "project", "session", "all"], "description": "Defaults to project for writes and all current scopes for reads" },
                    "kind": { "type": "string", "enum": ["fact", "preference", "decision", "instruction", "summary"] },
                    "evidence": { "type": "string", "description": "Why this memory is supported by the current conversation" },
                    "reason": { "type": "string", "description": "Reason for forgetting a record" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50 }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
            execution_mode: ToolExecutionMode::Sequential,
            prompt_snippet: Some("Use memory only for durable, user-approved facts, preferences, decisions, instructions, or summaries. Do not capture secrets or transient task details.".to_string()),
            prompt_guidelines: vec![
                "When the user explicitly asks you to remember, correct, or forget something, call this tool instead of merely claiming it was saved.".to_string(),
                "Use project scope by default; use user scope only for cross-project preferences, and session scope only for temporary session-specific facts.".to_string(),
            ],
        }
    }

    async fn execute(
        &self,
        context: ToolContext,
        tool_call_id: ToolCallId,
        input: Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        context.signal().check().map_err(|_| ToolError::Aborted)?;
        match required_string(&input, "action")? {
            "remember" => {
                self.write_record(&context, &tool_call_id, &input, None)
                    .await
            }
            "correct" => {
                let target = required_string(&input, "targetId")?;
                self.write_record(&context, &tool_call_id, &input, Some(target))
                    .await
            }
            "forget" => self.forget(&context, &tool_call_id, &input).await,
            "list" => self.query(&context, &input, false).await,
            "search" => self.query(&context, &input, true).await,
            value => Err(ToolError::InvalidArguments(format!(
                "unsupported memory action: {value}"
            ))),
        }
    }
}

async fn persist_then_apply(
    context: &ToolContext,
    runtime: &LocalMemoryRuntime,
    mutation: &MemoryMutation,
) -> Result<bool, ToolError> {
    mutation
        .validate()
        .map_err(|error| ToolError::InvalidArguments(error.to_string()))?;
    let data = serde_json::to_value(mutation).map_err(execution)?;
    context
        .session
        .append_entry(MEMORY_EVENT_TYPE, Some(data))?;
    // The journal append is the commit point. A failed derived-index update is
    // reported as pending and repaired by lifecycle reconciliation.
    Ok(runtime.apply(vec![mutation.clone()]).await.is_ok())
}

fn write_scope(
    scope: Option<&str>,
    runtime: &LocalMemoryRuntime,
    session_id: &str,
) -> Result<MemoryScope, ToolError> {
    match scope.unwrap_or("project") {
        "user" => Ok(MemoryScope::User),
        "project" => Ok(runtime.project_scope()),
        "session" => Ok(MemoryScope::Session {
            session_id: session_id.to_string(),
        }),
        "all" => Err(ToolError::InvalidArguments(
            "all is only valid for memory reads".to_string(),
        )),
        value => Err(ToolError::InvalidArguments(format!(
            "unsupported memory scope: {value}"
        ))),
    }
}

fn read_scopes(
    scope: Option<&str>,
    runtime: &LocalMemoryRuntime,
    session_id: &str,
) -> Result<Vec<MemoryScope>, ToolError> {
    match scope.unwrap_or("all") {
        "all" => Ok(runtime.scopes(session_id)),
        value => write_scope(Some(value), runtime, session_id).map(|scope| vec![scope]),
    }
}

fn memory_kind(kind: Option<&str>) -> Result<MemoryKind, ToolError> {
    match kind.unwrap_or("fact") {
        "fact" => Ok(MemoryKind::Fact),
        "preference" => Ok(MemoryKind::Preference),
        "decision" => Ok(MemoryKind::Decision),
        "instruction" => Ok(MemoryKind::Instruction),
        "summary" => Ok(MemoryKind::Summary),
        value => Err(ToolError::InvalidArguments(format!(
            "unsupported memory kind: {value}"
        ))),
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}
