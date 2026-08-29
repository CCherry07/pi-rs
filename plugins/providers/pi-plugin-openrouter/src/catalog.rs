use std::collections::BTreeMap;

use pi_core::{ModelCost, ModelInput, ModelSpec};
use serde_json::{Value, json};

use crate::{OPENROUTER_BASE_URL, OPENROUTER_PROVIDER_ID};

pub fn openrouter_models() -> Vec<ModelSpec> {
    vec![
        model(
            "anthropic/claude-sonnet-4.6",
            "Anthropic: Claude Sonnet 4.6",
            true,
            true,
            1_000_000,
            128_000,
            (3.0, 15.0, 0.3, 3.75),
            map(&[
                ("off", Some("none")),
                ("minimal", None),
                ("low", Some("low")),
                ("medium", Some("medium")),
                ("high", Some("high")),
                ("xhigh", None),
                ("max", Some("max")),
            ]),
            json!({"thinkingFormat":"openrouter","cacheControlFormat":"anthropic"}),
        ),
        model(
            "openai/gpt-5.4",
            "OpenAI: GPT-5.4",
            true,
            true,
            1_050_000,
            128_000,
            (2.5, 15.0, 0.25, 0.0),
            map(&[
                ("off", Some("none")),
                ("minimal", None),
                ("low", Some("low")),
                ("medium", Some("medium")),
                ("high", Some("high")),
                ("xhigh", Some("xhigh")),
                ("max", None),
            ]),
            json!({"thinkingFormat":"openrouter"}),
        ),
        model(
            "google/gemini-3.1-pro-preview",
            "Google: Gemini 3.1 Pro Preview",
            true,
            true,
            1_048_576,
            65_536,
            (2.0, 12.0, 0.2, 0.375),
            map(&[
                ("off", None),
                ("minimal", None),
                ("low", Some("low")),
                ("medium", Some("medium")),
                ("high", Some("high")),
                ("xhigh", None),
                ("max", None),
            ]),
            json!({"supportsDeveloperRole":false,"thinkingFormat":"openrouter"}),
        ),
        model(
            "moonshotai/kimi-k2.5",
            "MoonshotAI: Kimi K2.5",
            true,
            true,
            262_144,
            4_096,
            (0.41, 2.06, 0.07, 0.0),
            BTreeMap::new(),
            json!({"supportsDeveloperRole":false,"thinkingFormat":"openrouter"}),
        ),
        model(
            "deepseek/deepseek-v3.2",
            "DeepSeek: DeepSeek V3.2",
            true,
            false,
            163_840,
            65_536,
            (0.269, 0.4, 0.1345, 0.0),
            BTreeMap::new(),
            json!({"supportsDeveloperRole":false,"thinkingFormat":"openrouter"}),
        ),
        model(
            "x-ai/grok-4.6",
            "SpaceXAI: Grok 4.6",
            true,
            true,
            500_000,
            450_000,
            (2.0, 6.0, 0.5, 0.0),
            map(&[
                ("off", None),
                ("minimal", None),
                ("low", Some("low")),
                ("medium", Some("medium")),
                ("high", Some("high")),
                ("xhigh", Some("xhigh")),
                ("max", None),
            ]),
            json!({"supportsDeveloperRole":false,"thinkingFormat":"openrouter"}),
        ),
        model(
            "z-ai/glm-5",
            "Z.ai: GLM 5",
            true,
            false,
            198_000,
            128_000,
            (0.6, 1.9, 0.119, 0.0),
            BTreeMap::new(),
            json!({"supportsDeveloperRole":false,"thinkingFormat":"openrouter"}),
        ),
        model(
            "~openai/gpt-latest",
            "OpenAI GPT Latest",
            true,
            true,
            1_050_000,
            128_000,
            (2.0, 10.0, 0.2, 2.5),
            map(&[
                ("off", Some("none")),
                ("minimal", None),
                ("low", Some("low")),
                ("medium", Some("medium")),
                ("high", Some("high")),
                ("xhigh", Some("xhigh")),
                ("max", Some("max")),
            ]),
            json!({"supportsDeveloperRole":false,"thinkingFormat":"openrouter"}),
        ),
    ]
}

#[allow(clippy::too_many_arguments)]
fn model(
    id: &str,
    name: &str,
    reasoning: bool,
    image: bool,
    context_window: u64,
    max_tokens: u64,
    cost: (f64, f64, f64, f64),
    thinking_level_map: BTreeMap<String, Option<String>>,
    compat: Value,
) -> ModelSpec {
    let mut model = ModelSpec::new(OPENROUTER_PROVIDER_ID, id, name, "openai-completions");
    model.base_url = Some(OPENROUTER_BASE_URL.to_string());
    model.reasoning = reasoning;
    if image {
        model.input.push(ModelInput::Image);
    }
    model.cost = ModelCost {
        input: cost.0,
        output: cost.1,
        cache_read: cost.2,
        cache_write: cost.3,
        tiers: Vec::new(),
    };
    model.context_window = context_window;
    model.max_tokens = max_tokens;
    model.thinking_level_map = thinking_level_map;
    model.compat = Some(compat);
    model
}

fn map(values: &[(&str, Option<&str>)]) -> BTreeMap<String, Option<String>> {
    values
        .iter()
        .map(|(level, value)| (level.to_string(), value.map(str::to_string)))
        .collect()
}
