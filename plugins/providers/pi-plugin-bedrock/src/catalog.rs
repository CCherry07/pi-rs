use pi_core::{ModelCost, ModelInput, ModelSpec};
use serde_json::json;

use crate::{AMAZON_BEDROCK_PROVIDER_ID, BEDROCK_CONVERSE_STREAM_API, DEFAULT_BEDROCK_BASE_URL};

pub fn amazon_bedrock_models() -> Vec<ModelSpec> {
    vec![
        model(
            "amazon.nova-2-lite-v1:0",
            "Nova 2 Lite",
            true,
            true,
            0.33,
            2.75,
            0.0,
            0.0,
            128_000,
            4_096,
        ),
        model(
            "amazon.nova-pro-v1:0",
            "Nova Pro",
            false,
            true,
            0.8,
            3.2,
            0.2,
            0.0,
            300_000,
            8_192,
        ),
        claude(
            "anthropic.claude-haiku-4-5-20251001-v1:0",
            "Claude Haiku 4.5",
            1.0,
            5.0,
            0.1,
            1.25,
            200_000,
            64_000,
        ),
        claude(
            "anthropic.claude-opus-4-6-v1",
            "Claude Opus 4.6",
            5.0,
            25.0,
            0.5,
            6.25,
            1_000_000,
            128_000,
        )
        .thinking("max", Some("max")),
        claude(
            "anthropic.claude-sonnet-4-6",
            "Claude Sonnet 4.6",
            3.0,
            15.0,
            0.3,
            3.75,
            1_000_000,
            64_000,
        )
        .thinking("max", Some("max")),
        claude(
            "us.anthropic.claude-haiku-4-5-20251001-v1:0",
            "Claude Haiku 4.5 (US)",
            1.0,
            5.0,
            0.1,
            1.25,
            200_000,
            64_000,
        ),
        claude(
            "us.anthropic.claude-opus-4-6-v1",
            "Claude Opus 4.6 (US)",
            5.0,
            25.0,
            0.5,
            6.25,
            1_000_000,
            128_000,
        )
        .thinking("max", Some("max")),
        claude(
            "us.anthropic.claude-sonnet-4-6",
            "Claude Sonnet 4.6 (US)",
            3.0,
            15.0,
            0.3,
            3.75,
            1_000_000,
            64_000,
        )
        .thinking("max", Some("max")),
        model(
            "deepseek.v3.2",
            "DeepSeek-V3.2",
            true,
            false,
            0.62,
            1.85,
            0.0,
            0.0,
            163_840,
            81_920,
        )
        .strict(),
        model(
            "openai.gpt-oss-120b-1:0",
            "gpt-oss-120b",
            true,
            false,
            0.15,
            0.6,
            0.0,
            0.0,
            128_000,
            16_384,
        )
        .strict(),
        model(
            "qwen.qwen3-coder-480b-a35b-v1:0",
            "Qwen3 Coder 480B A35B Instruct",
            false,
            false,
            0.22,
            1.8,
            0.0,
            0.0,
            131_072,
            65_536,
        )
        .strict(),
    ]
    .into_iter()
    .map(ModelBuilder::finish)
    .collect()
}

#[allow(clippy::too_many_arguments)]
fn claude(
    id: &str,
    name: &str,
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
    context_window: u64,
    max_tokens: u64,
) -> ModelBuilder {
    model(
        id,
        name,
        true,
        true,
        input,
        output,
        cache_read,
        cache_write,
        context_window,
        max_tokens,
    )
    .strict()
}

#[allow(clippy::too_many_arguments)]
fn model(
    id: &str,
    name: &str,
    reasoning: bool,
    images: bool,
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
    context_window: u64,
    max_tokens: u64,
) -> ModelBuilder {
    let mut model = ModelSpec::new(
        AMAZON_BEDROCK_PROVIDER_ID,
        id,
        name,
        BEDROCK_CONVERSE_STREAM_API,
    );
    model.base_url = Some(DEFAULT_BEDROCK_BASE_URL.to_string());
    model.reasoning = reasoning;
    model.input = if images {
        vec![ModelInput::Text, ModelInput::Image]
    } else {
        vec![ModelInput::Text]
    };
    model.cost = ModelCost {
        input,
        output,
        cache_read,
        cache_write,
        tiers: Vec::new(),
    };
    model.context_window = context_window;
    model.max_tokens = max_tokens;
    ModelBuilder(model)
}

struct ModelBuilder(ModelSpec);

impl ModelBuilder {
    fn thinking(mut self, level: &str, value: Option<&str>) -> Self {
        self.0
            .thinking_level_map
            .insert(level.to_string(), value.map(str::to_string));
        self
    }

    fn strict(mut self) -> Self {
        self.0.compat = Some(json!({"supportsStrictMode": true}));
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
    fn catalog_includes_claude_nova_and_open_models() {
        let models = amazon_bedrock_models();
        assert!(
            models
                .iter()
                .any(|model| model.id.as_str().starts_with("anthropic."))
        );
        assert!(
            models
                .iter()
                .any(|model| model.id.as_str().starts_with("amazon.nova"))
        );
        assert!(
            models
                .iter()
                .any(|model| model.id.as_str().starts_with("openai."))
        );
        assert!(
            models
                .iter()
                .all(|model| model.api == BEDROCK_CONVERSE_STREAM_API)
        );
    }
}
