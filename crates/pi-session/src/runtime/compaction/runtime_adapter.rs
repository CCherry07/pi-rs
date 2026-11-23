//! Adapts the generation runtime to the compaction completion contract.

use async_trait::async_trait;
use pi_core::{AbortSignal, ContentBlock, Message, StopReason, UserMessage};
use pi_runtime::{PiRuntime, RuntimeCompletionRequest, RuntimeError};

use super::algorithm::{
    CompactionCompleter, CompactionCompletion, CompactionCompletionRequest, CompactionError,
    CompactionModelCapabilities, summarization_failure,
};

#[async_trait]
impl CompactionCompleter for PiRuntime {
    fn model_capabilities(&self) -> CompactionModelCapabilities {
        let state = self.agent().state();
        self.model(&state.provider_id, &state.model_id).map_or(
            CompactionModelCapabilities {
                reasoning: false,
                max_output_tokens: None,
            },
            |model| CompactionModelCapabilities {
                reasoning: model.reasoning,
                max_output_tokens: (model.max_tokens > 0).then_some(model.max_tokens),
            },
        )
    }

    async fn complete_compaction(
        &self,
        request: CompactionCompletionRequest,
        signal: AbortSignal,
    ) -> Result<CompactionCompletion, CompactionError> {
        compaction_completion(
            self.complete(
                self.prepare_summarization_request(compaction_request(self, request)),
                signal,
            )
            .await,
        )
    }
}

pub(super) fn compaction_request(
    runtime: &PiRuntime,
    request: CompactionCompletionRequest,
) -> RuntimeCompletionRequest {
    RuntimeCompletionRequest {
        system_prompt: request.system_prompt,
        messages: vec![Message::User(UserMessage::text(
            request.prompt,
            crate::now_ms(),
        ))],
        model: None,
        thinking_level: request.thinking_level,
        thinking_budgets: runtime.agent().thinking_budgets(),
        max_output_tokens: request.max_output_tokens,
        cache_retention: None,
        routing_session_id: None,
    }
}

pub(super) fn compaction_completion(
    response: Result<pi_core::AssistantMessage, RuntimeError>,
) -> Result<CompactionCompletion, CompactionError> {
    let response = response.map_err(|error| match error {
        RuntimeError::Aborted => CompactionError::Aborted("summarization aborted".into()),
        other => CompactionError::SummarizationFailed(other.to_string()),
    })?;
    if response.stop_reason == StopReason::Aborted {
        return Err(CompactionError::Aborted(
            response
                .error_message
                .unwrap_or_else(|| "summarization aborted".into()),
        ));
    }
    if let Some(error) = summarization_failure(&response, "Summarization") {
        return Err(CompactionError::SummarizationFailed(error));
    }
    Ok(CompactionCompletion {
        text: response
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        usage: response.usage,
    })
}
