use pi_core::{ModelCost, ModelCostTier, ModelInput, ModelSpec};
use serde_json::{Value, json};

use crate::{COPILOT_BASE_URL, GITHUB_COPILOT_PROVIDER_ID};

const ANTHROPIC_MESSAGES_API: &str = "anthropic-messages";
const OPENAI_COMPLETIONS_API: &str = "openai-completions";
const OPENAI_RESPONSES_API: &str = "openai-responses";

pub fn github_copilot_models() -> Vec<ModelSpec> {
    vec![
        anthropic(
            "claude-haiku-4.5",
            "Claude Haiku 4.5 (latest)",
            1.0,
            5.0,
            0.1,
            1.25,
            200_000,
            64_000,
        ),
        anthropic(
            "claude-opus-4.6",
            "Claude Opus 4.6",
            5.0,
            25.0,
            0.5,
            6.25,
            1_000_000,
            32_000,
        )
        .thinking("max", Some("max"))
        .compat(json!({"forceAdaptiveThinking": true})),
        anthropic(
            "claude-sonnet-4.6",
            "Claude Sonnet 4.6",
            3.0,
            15.0,
            0.3,
            3.75,
            1_000_000,
            32_000,
        )
        .thinking("minimal", Some("low"))
        .thinking("max", Some("max"))
        .compat(json!({"forceAdaptiveThinking": true})),
        anthropic(
            "claude-sonnet-5",
            "Claude Sonnet 5",
            2.0,
            10.0,
            0.2,
            2.5,
            1_000_000,
            128_000,
        )
        .thinking("xhigh", Some("xhigh"))
        .thinking("max", Some("max"))
        .compat(json!({"forceAdaptiveThinking": true})),
        completion(
            "gemini-3.1-pro-preview",
            "Gemini 3.1 Pro Preview",
            true,
            2.0,
            12.0,
            0.2,
            1_000_000,
            64_000,
        )
        .tier(200_000, 4.0, 18.0, 0.4, 0.0),
        completion("gpt-4.1", "GPT-4.1", false, 2.0, 8.0, 0.5, 128_000, 16_384),
        completion(
            "kimi-k2.7-code",
            "Kimi K2.7 Code",
            true,
            0.95,
            4.0,
            0.19,
            256_000,
            32_000,
        ),
        responses(
            "gpt-5-mini",
            "GPT-5 Mini",
            0.25,
            2.0,
            0.025,
            0.0,
            264_000,
            64_000,
        )
        .standard_reasoning(false),
        responses(
            "gpt-5.4", "GPT-5.4", 2.5, 15.0, 0.25, 0.0, 1_000_000, 128_000,
        )
        .standard_reasoning(true)
        .tier(272_000, 5.0, 22.5, 0.5, 0.0),
        responses(
            "gpt-5.4-mini",
            "GPT-5.4 mini",
            0.75,
            4.5,
            0.075,
            0.0,
            400_000,
            128_000,
        )
        .standard_reasoning(true),
        responses(
            "gpt-5.6-luna",
            "GPT-5.6 Luna",
            0.2,
            1.2,
            0.02,
            0.25,
            1_050_000,
            128_000,
        )
        .all_reasoning()
        .tier(200_000, 0.4, 1.8, 0.04, 0.5),
        responses(
            "gpt-5.6-sol",
            "GPT-5.6 Sol",
            2.0,
            10.0,
            0.2,
            2.5,
            1_050_000,
            128_000,
        )
        .all_reasoning()
        .tier(272_000, 4.0, 15.0, 0.4, 5.0),
        responses(
            "gpt-5.6-terra",
            "GPT-5.6 Terra",
            2.0,
            12.0,
            0.2,
            2.5,
            1_050_000,
            128_000,
        )
        .all_reasoning()
        .tier(272_000, 4.0, 18.0, 0.4, 5.0),
        responses("grok-4.6", "Grok 4.6", 2.0, 6.0, 0.5, 0.0, 500_000, 128_000)
            .thinking("off", None)
            .thinking("minimal", None)
            .thinking("low", Some("low"))
            .thinking("medium", Some("medium"))
            .thinking("high", Some("high"))
            .thinking("xhigh", Some("xhigh"))
            .thinking("max", None)
            .tier(200_000, 4.0, 12.0, 1.0, 0.0),
        responses(
            "mai-code-1.1-flash",
            "MAI-Code-1.1-Flash",
            0.2,
            1.2,
            0.02,
            0.0,
            256_000,
            128_000,
        )
        .thinking("off", None)
        .thinking("minimal", None)
        .thinking("low", Some("low"))
        .thinking("medium", Some("medium"))
        .thinking("high", Some("high"))
        .thinking("xhigh", None)
        .thinking("max", None),
    ]
    .into_iter()
    .map(ModelBuilder::finish)
    .collect()
}

#[allow(clippy::too_many_arguments)]
fn anthropic(
    id: &str,
    name: &str,
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
    context_window: u64,
    max_tokens: u64,
) -> ModelBuilder {
    ModelBuilder::new(
        id,
        name,
        ANTHROPIC_MESSAGES_API,
        true,
        ModelCost {
            input,
            output,
            cache_read,
            cache_write,
            tiers: Vec::new(),
        },
        context_window,
        max_tokens,
    )
}

#[allow(clippy::too_many_arguments)]
fn completion(
    id: &str,
    name: &str,
    reasoning: bool,
    input: f64,
    output: f64,
    cache_read: f64,
    context_window: u64,
    max_tokens: u64,
) -> ModelBuilder {
    ModelBuilder::new(
        id,
        name,
        OPENAI_COMPLETIONS_API,
        reasoning,
        ModelCost {
            input,
            output,
            cache_read,
            cache_write: 0.0,
            tiers: Vec::new(),
        },
        context_window,
        max_tokens,
    )
    .compat(json!({
        "supportsStore": false,
        "supportsDeveloperRole": false,
        "supportsReasoningEffort": false
    }))
}

#[allow(clippy::too_many_arguments)]
fn responses(
    id: &str,
    name: &str,
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
    context_window: u64,
    max_tokens: u64,
) -> ModelBuilder {
    ModelBuilder::new(
        id,
        name,
        OPENAI_RESPONSES_API,
        true,
        ModelCost {
            input,
            output,
            cache_read,
            cache_write,
            tiers: Vec::new(),
        },
        context_window,
        max_tokens,
    )
    .compat(json!({"supportsOpenAIGrammarTools": true}))
}

struct ModelBuilder(ModelSpec);

impl ModelBuilder {
    fn new(
        id: &str,
        name: &str,
        api: &str,
        reasoning: bool,
        cost: ModelCost,
        context_window: u64,
        max_tokens: u64,
    ) -> Self {
        let mut model = ModelSpec::new(GITHUB_COPILOT_PROVIDER_ID, id, name, api);
        model.base_url = Some(COPILOT_BASE_URL.to_string());
        model.reasoning = reasoning;
        model.input = vec![ModelInput::Text, ModelInput::Image];
        model.cost = cost;
        model.context_window = context_window;
        model.max_tokens = max_tokens;
        Self(model)
    }

    fn thinking(mut self, level: &str, value: Option<&str>) -> Self {
        self.0
            .thinking_level_map
            .insert(level.to_string(), value.map(str::to_string));
        self
    }

    fn standard_reasoning(self, xhigh: bool) -> Self {
        let mut model = self
            .thinking("off", None)
            .thinking("minimal", Some("low"))
            .thinking("low", Some("low"))
            .thinking("medium", Some("medium"))
            .thinking("high", Some("high"));
        if xhigh {
            model = model.thinking("xhigh", Some("xhigh"));
        } else {
            model = model.thinking("xhigh", None);
        }
        model.thinking("max", None)
    }

    fn all_reasoning(self) -> Self {
        self.standard_reasoning(true).thinking("max", Some("max"))
    }

    fn tier(
        mut self,
        input_tokens_above: u64,
        input: f64,
        output: f64,
        cache_read: f64,
        cache_write: f64,
    ) -> Self {
        self.0.cost.tiers.push(ModelCostTier {
            input_tokens_above,
            input,
            output,
            cache_read,
            cache_write,
        });
        self
    }

    fn compat(mut self, value: Value) -> Self {
        self.0.compat = Some(value);
        self
    }

    fn finish(self) -> ModelSpec {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_spans_all_three_copilot_protocols() {
        let models = github_copilot_models();
        assert!(
            models
                .iter()
                .any(|model| model.api == ANTHROPIC_MESSAGES_API)
        );
        assert!(
            models
                .iter()
                .any(|model| model.api == OPENAI_COMPLETIONS_API)
        );
        assert!(models.iter().any(|model| model.api == OPENAI_RESPONSES_API));
        assert!(models.iter().any(|model| model.id.as_str() == "gpt-5.4"));
    }
}
