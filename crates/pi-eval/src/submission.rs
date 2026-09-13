use std::sync::Arc;

use async_trait::async_trait;
use pi_core::{
    AgentPlugin, PluginId, RegisterContext, Tool, ToolCallId, ToolContext, ToolError,
    ToolExecutionMode, ToolResult, ToolSpec, ToolUpdateSink,
};
use serde_json::Value;

use crate::{EvalGrade, EvalGrader, EvalObservation, EvalTranscriptEvent};

#[derive(Debug, Clone)]
pub struct JsonSubmissionPlugin {
    tool: JsonSubmissionTool,
}

impl JsonSubmissionPlugin {
    pub fn new(
        name: impl Into<String>,
        label: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
    ) -> Self {
        let name = name.into();
        Self {
            tool: JsonSubmissionTool {
                prompt_snippet: Some(format!(
                    "Submit the final result with the {name} tool as validated structured data"
                )),
                name,
                label: label.into(),
                description: description.into(),
                parameters,
            },
        }
    }

    pub fn name(&self) -> &str {
        &self.tool.name
    }
}

#[pi_core::agent_plugin]
impl AgentPlugin for JsonSubmissionPlugin {
    fn id(&self) -> PluginId {
        PluginId::new(format!("pi-eval-submit-{}", self.tool.name))
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_core::Result<()> {
        context.register_tool(Arc::new(self.tool.clone()))
    }
}

#[derive(Debug, Clone)]
struct JsonSubmissionTool {
    name: String,
    label: String,
    description: String,
    parameters: Value,
    prompt_snippet: Option<String>,
}

#[async_trait]
impl Tool for JsonSubmissionTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.clone(),
            label: self.label.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
            execution_mode: ToolExecutionMode::Sequential,
            prompt_snippet: self.prompt_snippet.clone(),
            prompt_guidelines: Vec::new(),
        }
    }

    async fn execute(
        &self,
        _context: ToolContext,
        _tool_call_id: ToolCallId,
        input: Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        Ok(ToolResult {
            details: Some(input),
            terminate: true,
            ..ToolResult::text("Structured eval result submitted.")
        })
    }
}

#[derive(Debug, Clone)]
pub struct JsonSubmissionGrader {
    tool_name: String,
    pointer: String,
    expected: Value,
    required: bool,
}

impl JsonSubmissionGrader {
    pub fn new(tool_name: impl Into<String>, pointer: impl Into<String>, expected: Value) -> Self {
        Self {
            tool_name: tool_name.into(),
            pointer: pointer.into(),
            expected,
            required: true,
        }
    }

    pub fn required(mut self, required: bool) -> Self {
        self.required = required;
        self
    }
}

impl EvalGrader for JsonSubmissionGrader {
    fn name(&self) -> &str {
        "json_submission"
    }

    fn grade(&self, observation: &EvalObservation) -> EvalGrade {
        let calls = observation
            .transcript
            .iter()
            .filter_map(|event| match event {
                EvalTranscriptEvent::ToolCall {
                    id,
                    name,
                    arguments,
                } if name == &self.tool_name => Some((id, arguments)),
                _ => None,
            })
            .collect::<Vec<_>>();
        let failure = if calls.len() != 1 {
            Some(format!(
                "expected exactly one {} tool call, observed {}",
                self.tool_name,
                calls.len()
            ))
        } else {
            let (call_id, arguments) = calls[0];
            let result = observation.transcript.iter().find_map(|event| match event {
                EvalTranscriptEvent::ToolResult {
                    tool_call_id,
                    name,
                    details,
                    is_error,
                    ..
                } if tool_call_id == call_id && name == &self.tool_name => {
                    Some((details.as_ref(), *is_error))
                }
                _ => None,
            });
            match result {
                None => Some(format!(
                    "{} tool call has no matching result",
                    self.tool_name
                )),
                Some((_, true)) => Some(format!("{} tool result is an error", self.tool_name)),
                Some((details, false)) => {
                    let actual = details
                        .and_then(|value| value.pointer(&self.pointer))
                        .or_else(|| arguments.pointer(&self.pointer));
                    (actual != Some(&self.expected)).then(|| {
                        format!(
                            "expected {} at JSON pointer {}, received {:?}",
                            self.expected, self.pointer, actual
                        )
                    })
                }
            }
        };
        EvalGrade {
            grader: self.name().to_string(),
            score: if failure.is_none() { 1.0 } else { 0.0 },
            passed: failure.is_none(),
            required: self.required,
            rationale: failure.unwrap_or_else(|| {
                format!("{} submitted the expected structured value", self.tool_name)
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EvalUsage;

    #[test]
    fn grader_requires_one_successful_matching_submission() {
        let grader = JsonSubmissionGrader::new("submit", "/verdict", Value::from("match"));
        let observation = EvalObservation {
            system_prompt: None,
            final_response: String::new(),
            transcript: vec![
                EvalTranscriptEvent::ToolCall {
                    id: "call-1".to_string(),
                    name: "submit".to_string(),
                    arguments: serde_json::json!({"verdict": "match"}),
                },
                EvalTranscriptEvent::ToolResult {
                    tool_call_id: "call-1".to_string(),
                    name: "submit".to_string(),
                    content: "done".to_string(),
                    details: Some(serde_json::json!({"verdict": "match"})),
                    is_error: false,
                },
            ],
            workspace_changes: Vec::new(),
            usage: EvalUsage::default(),
            errors: Vec::new(),
        };
        assert!(grader.grade(&observation).passed);
    }
}
