#![forbid(unsafe_code)]

//! Generic discovery of deferred tools through the current run's visible catalog.
//!
//! Search requests additions through the ordinary tool-result contract. The agent loop owns
//! activation, and session machinery owns recording and restoring the resulting selection.

mod ranker;

pub use ranker::{
    Bm25Ranker, ToolSearchDocument, ToolSearchMatch, create_tool_search_document, tokenize,
};

use std::sync::Arc;

use async_trait::async_trait;
use pi_core::{PluginId, ToolCallId, ToolExecutionMode, ToolResult, ToolSpec};
use pi_plugin::{Plugin, RegisterContext, Tool, ToolContext, ToolError, ToolUpdateSink};
use serde_json::{Value, json};

pub const TOOL_SEARCH_TOOL_NAME: &str = "tool_search";
pub const DEFAULT_TOOL_SEARCH_LIMIT: usize = 8;
pub const TOOL_SEARCH_DESCRIPTION: &str = "# Tool discovery\n\nSearches over deferred tool metadata with BM25 and exposes matching tools for the next model call.\n\nSome of the tools, such as tools of MCP servers, may not have been provided to you upfront, and you should use this tool (`tool_search`) to search for the required tools. For MCP tool discovery, always use `tool_search`.";

/// Registers the generic search tool. Product composition chooses whether it starts active.
#[derive(Debug, Default)]
pub struct ToolSearchPlugin;

#[pi_plugin::plugin]
impl Plugin for ToolSearchPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("tool-search")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_tool(tool_search_tool())
    }
}

/// A standalone tool for hosts that assemble a registry without `ToolSearchPlugin`.
pub fn tool_search_tool() -> Arc<dyn Tool> {
    Arc::new(ToolSearchTool)
}

struct ToolSearchTool;

#[async_trait]
impl Tool for ToolSearchTool {
    fn model_only(&self) -> bool {
        true
    }

    fn reaches_tools(&self) -> bool {
        true
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            output_schema: None,
            constrained_sampling: None,
            name: TOOL_SEARCH_TOOL_NAME.into(),
            label: TOOL_SEARCH_TOOL_NAME.into(),
            description: TOOL_SEARCH_DESCRIPTION.into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Search query for deferred tools."},
                    "limit": {"type": "number", "description": "Maximum number of tools to return. Defaults to 8."}
                },
                "required": ["query"]
            }),
            // A search changes the next request's active selection.
            execution_mode: ToolExecutionMode::Sequential,
            prompt_snippet: Some(
                "Search for tools that are not loaded yet and load the matches".into(),
            ),
            prompt_guidelines: Vec::new(),
        }
    }

    fn validate_arguments(&self, input: &Value) -> Result<(), ToolError> {
        parse_input(input).map(|_| ())
    }

    async fn execute(
        &self,
        context: ToolContext,
        _tool_call_id: ToolCallId,
        input: Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        context.signal().check().map_err(|_| ToolError::Aborted)?;
        let (query, limit) = parse_input(&input)?;
        let candidates = context.discoverable_tools();
        let documents = candidates
            .iter()
            .map(|tool| create_tool_search_document(&tool.spec, tool.discovery.namespace.as_ref()))
            .collect::<Vec<_>>();
        let matches = Bm25Ranker::default().rank(query, &documents, limit);
        let loaded = matches
            .iter()
            .map(|found| found.name.clone())
            .collect::<Vec<_>>();
        let text = if loaded.is_empty() {
            "No matching tools found.".into()
        } else {
            let lines = matches
                .iter()
                .map(|found| {
                    let description = candidates
                        .iter()
                        .find(|tool| tool.spec.name == found.name)
                        .map_or("", |tool| tool.spec.description.trim());
                    let first_line = description
                        .split('\n')
                        .next()
                        .unwrap_or("")
                        .trim_end_matches('\r');
                    format!("- {}: {first_line}", found.name)
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "Loaded {} tool{}. They are available from your next call:\n{lines}",
                loaded.len(),
                if loaded.len() == 1 { "" } else { "s" }
            )
        };
        context.signal().check().map_err(|_| ToolError::Aborted)?;
        Ok(ToolResult {
            structured_content: None,
            details: Some(json!({"loaded": loaded})),
            added_tool_names: Some(loaded),
            ..ToolResult::text(text)
        })
    }
}

fn parse_input(input: &Value) -> Result<(&str, usize), ToolError> {
    let object = input
        .as_object()
        .ok_or_else(|| ToolError::InvalidArguments("arguments must be a JSON object".into()))?;
    let query = object
        .get("query")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::InvalidArguments("query must be a string".into()))?;
    if query.trim().is_empty() {
        return Err(ToolError::InvalidArguments(
            "query must not be empty".into(),
        ));
    }
    let limit = match object.get("limit") {
        None => DEFAULT_TOOL_SEARCH_LIMIT,
        Some(number) => {
            let limit = number
                .as_f64()
                .filter(|value| value.is_finite() && *value > 0.0 && value.fract() == 0.0)
                .ok_or_else(|| {
                    ToolError::InvalidArguments("limit must be a positive integer".into())
                })?;
            // JavaScript's slice accepts integers beyond the array length. Saturating the
            // conversion keeps that behavior without rejecting a valid, very large limit.
            limit as usize
        }
    };
    Ok((query, limit))
}

#[cfg(test)]
mod tests;
