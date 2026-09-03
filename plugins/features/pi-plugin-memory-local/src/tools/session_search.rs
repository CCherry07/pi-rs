use async_trait::async_trait;
use pi_core::{
    Tool, ToolCallId, ToolContext, ToolError, ToolExecutionMode, ToolResult, ToolSpec,
    ToolUpdateSink,
};
use serde_json::{Value, json};

use super::output::{
    MAX_RENDERED_TEXT_BYTES, bounded_lines, execution, query_limit, required_string,
};
use crate::SessionSearchQuery;
use crate::runtime::{LocalMemoryRuntime, truncate_utf8};

pub(super) struct SessionSearchTool {
    runtime: LocalMemoryRuntime,
}

impl SessionSearchTool {
    pub(super) fn new(runtime: LocalMemoryRuntime) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl Tool for SessionSearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "session_search".to_string(),
            label: "Session search".to_string(),
            description: "Search user and assistant text from active branches of indexed sessions in the current project.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "scope": { "type": "string", "enum": ["project", "session"], "description": "Defaults to project" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50 }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            execution_mode: ToolExecutionMode::Parallel,
            prompt_snippet: Some("Search prior conversation text when the current task refers to earlier work not present in context.".to_string()),
            prompt_guidelines: Vec::new(),
        }
    }

    async fn execute(
        &self,
        context: ToolContext,
        _tool_call_id: ToolCallId,
        input: Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        context.signal().check().map_err(|_| ToolError::Aborted)?;
        let query = required_string(&input, "query")?.trim();
        let snapshot = context.session.snapshot()?;
        self.runtime
            .reconcile_snapshot(snapshot.clone())
            .await
            .map_err(execution)?;
        let session_id = match input.get("scope").and_then(Value::as_str) {
            None | Some("project") => None,
            Some("session") => Some(snapshot.id().to_string()),
            Some(scope) => {
                return Err(ToolError::InvalidArguments(format!(
                    "unsupported session search scope: {scope}"
                )));
            }
        };
        let hits = self
            .runtime
            .search_sessions(SessionSearchQuery {
                text: query.to_string(),
                project_key: self.runtime.project_key().to_string(),
                session_id,
                limit: query_limit(&input)?,
            })
            .await
            .map_err(execution)?;
        if hits.is_empty() {
            return Ok(ToolResult::text("No matching session text."));
        }
        let lines = hits
            .iter()
            .map(|hit| {
                format!(
                    "- session={} entry={} role={}\n  {}",
                    hit.session_id,
                    hit.entry_id,
                    hit.role,
                    truncate_utf8(&hit.text, MAX_RENDERED_TEXT_BYTES)
                )
            })
            .collect::<Vec<_>>();
        let rendered = bounded_lines(&lines);
        let mut result = ToolResult::text(rendered);
        result.details = Some(json!({
            "matches": hits.iter().map(|hit| json!({
                "sessionId": hit.session_id,
                "entryId": hit.entry_id,
                "role": hit.role,
                "timestampMs": hit.timestamp_ms,
                "score": hit.score
            })).collect::<Vec<_>>()
        }));
        Ok(result)
    }
}
