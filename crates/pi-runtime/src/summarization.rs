//! Policy for one-off summaries, separate from ordinary Agent/provider defaults.

use crate::{PiRuntime, RuntimeCompletionRequest};
use pi_core::{ContentBlock, Message, ModelSelection, StopReason};
use pi_plugin::CacheRetention;

impl PiRuntime {
    /// Prepare one tool-free summary before retrying it. Explicit model, routing and
    /// output-cap choices are retained; otherwise use the active model and fresh routing.
    pub fn prepare_summarization_request(
        &self,
        mut request: RuntimeCompletionRequest,
    ) -> RuntimeCompletionRequest {
        let selection = request.model.get_or_insert_with(|| {
            let state = self.agent().state();
            ModelSelection {
                provider: state.provider_id,
                model_id: state.model_id,
            }
        });
        if let Some(model) = self.model(&selection.provider, &selection.model_id) {
            request.thinking_level = model.clamp_thinking_level(request.thinking_level);
            let cap = request.max_output_tokens.unwrap_or(model.max_tokens);
            request.max_output_tokens = Some(fit_output(
                cap,
                model.context_window,
                estimate_input(&request),
            ));
        }
        // Never inherit the parent routing identity for a one-off summary. Resolve
        // this once, before retries, so all attempts of the same summary share it.
        request
            .routing_session_id
            .get_or_insert_with(|| uuid::Uuid::now_v7().to_string());
        request.cache_retention = Some(CacheRetention::None);
        request
    }
}

// Current Pi simple-options.ts. Zero/unknown window disables the context clamp;
// with a known window the caller/model cap (including zero) remains authoritative.
fn fit_output(cap: u64, window: u64, input: u64) -> u64 {
    if window == 0 {
        return cap.max(1);
    }
    let margin = 4_096.max(input.saturating_mul(15).div_ceil(100));
    let mut room = window.saturating_sub(input).saturating_sub(margin);
    if room < 32_768 {
        room = 32_768.min(window.saturating_sub(input).saturating_sub(4_096));
    }
    cap.min(room.max(1))
}

// Match ai/utils/estimate.ts on the normalized tool-free transcript, including
// UTF-16 lengths and the newest usage whose timestamp still covers the prefix.
fn estimate_input(request: &RuntimeCompletionRequest) -> u64 {
    let mut latest_prefix = if request.system_prompt.is_empty() {
        i64::MIN
    } else {
        0
    };
    let mut tokens = text_tokens(&request.system_prompt);
    for original in &request.messages {
        let message = original.clone().into_provider_message();
        let timestamp = match &message {
            Message::System(m) => m.timestamp_ms,
            Message::User(m) => m.timestamp_ms,
            Message::Assistant(m) => m.timestamp_ms,
            Message::ToolResult(m) => m.timestamp_ms,
            Message::Custom(_) => unreachable!("custom messages are projected"),
        };
        let usage = if let Message::Assistant(m) = &message {
            let usage = &m.usage;
            let total = if usage.total_tokens != 0 {
                usage.total_tokens
            } else {
                usage
                    .input
                    .saturating_add(usage.output)
                    .saturating_add(usage.cache_read)
                    .saturating_add(usage.cache_write)
            };
            (timestamp >= latest_prefix
                && !matches!(m.stop_reason, StopReason::Aborted | StopReason::Error)
                && total > 0)
                .then_some(total)
        } else {
            None
        };
        tokens = usage.unwrap_or_else(|| tokens.saturating_add(message_tokens(&message)));
        latest_prefix = latest_prefix.max(timestamp);
    }
    tokens
}

fn text_chars(text: &str) -> u64 {
    text.encode_utf16().count() as u64
}
fn chars_tokens(chars: u64) -> u64 {
    chars.saturating_mul(2).div_ceil(7)
}
fn text_tokens(text: &str) -> u64 {
    chars_tokens(text_chars(text))
}

fn message_tokens(message: &Message) -> u64 {
    let (blocks, assistant) = match message {
        Message::System(m) => {
            let declarations = m
                .tools_added
                .as_ref()
                .filter(|v| !v.is_empty())
                .map_or(0, |v| {
                    text_tokens(&serde_json::to_string(v).expect("serializable declarations"))
                });
            let removals = m
                .tools_removed
                .as_ref()
                .filter(|v| !v.is_empty())
                .map_or(0, |v| {
                    text_tokens(&serde_json::to_string(v).expect("serializable references"))
                });
            return text_tokens(&m.render())
                .saturating_add(declarations)
                .saturating_add(removals);
        }
        Message::User(m) => (&m.content, false),
        Message::ToolResult(m) => (&m.content, false),
        Message::Assistant(m) => (&m.content, true),
        Message::Custom(_) => unreachable!("custom messages are projected"),
    };
    chars_tokens(
        blocks
            .iter()
            .map(|block| match block {
                ContentBlock::Text(text) => text_chars(&text.text),
                ContentBlock::Thinking(thinking) if assistant => text_chars(&thinking.thinking),
                ContentBlock::ToolCall(call) if assistant => {
                    text_chars(&call.name).saturating_add(text_chars(&call.arguments.to_string()))
                }
                _ => 4_800,
            })
            .sum(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::{AbortHandle, Message, ModelSpec, ThinkingLevel, UserMessage};
    use pi_plugin::{CacheRetention, ProviderPlugin, ProviderRegisterContext};
    use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};

    struct Catalog(ModelSpec);

    #[pi_plugin::provider_plugin]
    impl ProviderPlugin for Catalog {
        fn id(&self) -> pi_core::PluginId {
            "summary-catalog".into()
        }
        fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_plugin::Result<()> {
            context.register_model(self.0.clone())
        }
    }

    fn request() -> RuntimeCompletionRequest {
        RuntimeCompletionRequest {
            system_prompt: "Summarize".into(),
            messages: vec![Message::User(UserMessage::text("history", 1))],
            model: None,
            thinking_level: ThinkingLevel::Medium,
            thinking_budgets: None,
            max_output_tokens: None,
            cache_retention: None,
            routing_session_id: None,
        }
    }

    fn model() -> ModelSpec {
        let mut model = ModelSpec::new("scripted", "test", "Test", "scripted");
        model.reasoning = true;
        model.max_tokens = 100_000;
        model.context_window = 40_000;
        model
    }

    #[tokio::test]
    async fn summary_policy_reaches_provider_without_changing_normal_completions() {
        let scripted =
            ScriptedProviderPlugin::scripted((0..4).map(|_| ScriptedTurn::Text("ok".into())));
        let provider = scripted.provider();
        let runtime = PiRuntime::builder()
            .provider_plugin(scripted)
            .provider_plugin(Catalog(model()))
            .build()
            .unwrap();
        runtime.agent().set_session_id(Some("parent".into()));
        let first = runtime.prepare_summarization_request(request());
        let second = runtime.prepare_summarization_request(request());
        let mut explicit = request();
        explicit.routing_session_id = Some("explicit".into());
        explicit.cache_retention = Some(CacheRetention::Long);
        explicit.max_output_tokens = Some(500);
        let explicit = runtime.prepare_summarization_request(explicit);
        for request in [first, second, explicit, request()] {
            runtime
                .complete(request, AbortHandle::new().1)
                .await
                .unwrap();
        }
        let seen = provider.requests();
        for summary in &seen[..3] {
            assert_eq!(summary.cache_retention, Some(CacheRetention::None));
            assert_eq!(summary.thinking_level, ThinkingLevel::Medium);
            assert!(summary.tools.is_empty());
        }
        assert_ne!(seen[0].session_id, seen[1].session_id);
        for summary in &seen[..2] {
            assert_ne!(summary.session_id.as_deref(), Some("parent"));
            assert!(uuid::Uuid::parse_str(summary.session_id.as_deref().unwrap()).is_ok());
            // ceil(9/3.5) + ceil(7/3.5) = 5 input tokens, plus the fixed margin.
            assert_eq!(summary.max_output_tokens, Some(40_000 - 5 - 4_096));
        }
        assert_eq!(seen[2].session_id.as_deref(), Some("explicit"));
        assert_eq!(seen[2].max_output_tokens, Some(500));
        assert_eq!(seen[3].session_id.as_deref(), Some("parent"));
        assert_eq!(seen[3].cache_retention, None);
        assert_eq!(seen[3].max_output_tokens, None);
    }

    #[tokio::test]
    async fn summary_retries_keep_prepared_routing_and_model_policy() {
        let scripted = ScriptedProviderPlugin::scripted([
            ScriptedTurn::Error("stream ended before a terminal response event".into()),
            ScriptedTurn::Text("ok".into()),
        ]);
        let provider = scripted.provider();
        let runtime = PiRuntime::builder()
            .provider_plugin(scripted)
            .provider_plugin(Catalog(model()))
            .build()
            .unwrap();
        let request = runtime.prepare_summarization_request(request());
        assert_eq!(request.model.as_ref().unwrap().model_id.as_str(), "test");
        runtime
            .complete_with_retry(
                request,
                AbortHandle::new().1,
                Some(crate::CompletionRetryPolicy {
                    enabled: true,
                    max_retries: 1,
                    base_delay_ms: 0,
                }),
                |_| {},
            )
            .await
            .unwrap();
        let seen = provider.requests();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].session_id, seen[1].session_id);
        assert_eq!(seen[0].max_output_tokens, seen[1].max_output_tokens);
        assert_eq!(seen[1].cache_retention, Some(CacheRetention::None));
    }

    #[test]
    fn summary_model_override_zero_limits_and_missing_catalog_are_explicit() {
        let mut chosen = model();
        chosen.id = "other".into();
        chosen.reasoning = false;
        chosen.max_tokens = 0;
        chosen.context_window = 0;
        let runtime = PiRuntime::builder()
            .provider_plugin(ScriptedProviderPlugin::scripted([]))
            .provider_plugin(Catalog(chosen))
            .build()
            .unwrap();
        let mut req = request();
        req.model = Some(pi_core::ModelSelection {
            provider: "scripted".into(),
            model_id: "other".into(),
        });
        let prepared = runtime.prepare_summarization_request(req.clone());
        assert_eq!(prepared.model, req.model);
        assert_eq!(prepared.thinking_level, ThinkingLevel::Off);
        assert_eq!(prepared.max_output_tokens, Some(1));
        req.max_output_tokens = Some(777);
        assert_eq!(
            runtime.prepare_summarization_request(req).max_output_tokens,
            Some(777)
        );
        // No metadata: do not invent a model cap for an unregistered model.
        assert_eq!(
            runtime
                .prepare_summarization_request(request())
                .max_output_tokens,
            None
        );
        assert_eq!(super::fit_output(0, 20_000, 100), 0);
    }

    #[test]
    fn summary_fit_uses_only_usage_that_covers_the_current_prefix() {
        let runtime = PiRuntime::builder()
            .provider_plugin(ScriptedProviderPlugin::scripted([]))
            .provider_plugin(Catalog(model()))
            .build()
            .unwrap();
        let mut req = request();
        let usage = pi_core::Usage {
            input: 20_000,
            ..Default::default()
        };
        req.messages.push(serde_json::from_value(serde_json::json!({
            "role": "assistant", "content": [{"type":"text","text":"ok"}], "api":"scripted",
            "provider":"scripted", "model":"test", "usage":usage, "stopReason":"stop", "timestamp":2
        })).unwrap());
        req.messages
            .push(Message::User(UserMessage::text("tail", 3)));
        assert_eq!(
            runtime
                .prepare_summarization_request(req.clone())
                .max_output_tokens,
            Some(15_902)
        );
        // New summary inserted before an old response invalidates that old usage.
        req.messages[0] = Message::User(UserMessage::text("history", 4));
        assert_eq!(
            runtime.prepare_summarization_request(req).max_output_tokens,
            Some(35_896)
        );
    }

    #[test]
    fn summary_fit_matches_legacy_margins_and_utf16_estimates() {
        for (window, input, cap, expected) in [
            (1_048_576, 220_815, 943_718, 794_638),
            (1_048_576, 10_000, 1_048_576, 1_034_480),
            (1_048_576, 100_000, 64_000, 64_000),
            (1_048_576, 912_000, 943_718, 32_768),
            (1_048_576, 1_030_000, 943_718, 14_480),
            (32_768, 20_000, 32_768, 8_672),
            (1_048_576, 1_050_000, 943_718, 1),
        ] {
            let mut model = model();
            model.max_tokens = cap;
            model.context_window = window;
            let runtime = PiRuntime::builder()
                .provider_plugin(ScriptedProviderPlugin::scripted([]))
                .provider_plugin(Catalog(model))
                .build()
                .unwrap();
            let mut req = request();
            req.system_prompt.clear();
            // Seven UTF-16 units for every two tokens. Surrogate pairs count twice.
            req.messages = vec![Message::User(UserMessage::text(
                format!(
                    "{}{}",
                    "😀abcde".repeat(input / 2),
                    if input % 2 == 1 { "abc" } else { "" }
                ),
                1,
            ))];
            assert_eq!(
                runtime.prepare_summarization_request(req).max_output_tokens,
                Some(expected)
            );
        }
    }
}
