use pi_core::{ModelCost, ModelInput, ModelSpec};
use serde_json::json;

use crate::{AZURE_OPENAI_RESPONSES_API, AZURE_PROVIDER_ID};

/// Current high-value subset of Pi's Azure OpenAI Responses catalog.
pub fn azure_openai_models() -> Vec<ModelSpec> {
    vec![
        model(
            "gpt-4o",
            "GPT-4o",
            false,
            128_000,
            16_384,
            (2.5, 10.0, 1.25),
        ),
        model(
            "gpt-4.1",
            "GPT-4.1",
            false,
            1_047_576,
            32_768,
            (2.0, 8.0, 0.5),
        ),
        model(
            "gpt-5-mini",
            "GPT-5 Mini",
            true,
            400_000,
            128_000,
            (0.25, 2.0, 0.025),
        ),
        model(
            "gpt-5.2",
            "GPT-5.2",
            true,
            400_000,
            128_000,
            (1.75, 14.0, 0.175),
        ),
        model(
            "gpt-5.3-codex",
            "GPT-5.3 Codex",
            true,
            400_000,
            128_000,
            (1.75, 14.0, 0.175),
        ),
        model(
            "gpt-5.4",
            "GPT-5.4",
            true,
            1_050_000,
            128_000,
            (2.5, 15.0, 0.25),
        ),
        model(
            "gpt-5.4-mini",
            "GPT-5.4 mini",
            true,
            400_000,
            128_000,
            (0.75, 4.5, 0.075),
        ),
        model("o3", "o3", true, 200_000, 100_000, (2.0, 8.0, 0.5)),
        model(
            "o4-mini",
            "o4-mini",
            true,
            200_000,
            100_000,
            (1.1, 4.4, 0.275),
        ),
    ]
}

#[allow(clippy::too_many_arguments)]
fn model(
    id: &str,
    name: &str,
    reasoning: bool,
    context_window: u64,
    max_tokens: u64,
    cost: (f64, f64, f64),
) -> ModelSpec {
    let mut model = ModelSpec::new(AZURE_PROVIDER_ID, id, name, AZURE_OPENAI_RESPONSES_API);
    model.reasoning = reasoning;
    model.input = vec![ModelInput::Text, ModelInput::Image];
    model.cost = ModelCost {
        input: cost.0,
        output: cost.1,
        cache_read: cost.2,
        cache_write: 0.0,
        tiers: Vec::new(),
    };
    model.context_window = context_window;
    model.max_tokens = max_tokens;
    if reasoning {
        model.thinking_level_map.insert("off".to_string(), None);
        model.compat = Some(json!({"supportsOpenAIGrammarTools": true}));
    }
    model
}
