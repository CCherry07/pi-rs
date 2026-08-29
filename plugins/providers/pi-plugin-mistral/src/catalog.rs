use pi_core::{ModelCost, ModelInput, ModelSpec};

use crate::{MISTRAL_BASE_URL, MISTRAL_CONVERSATIONS_API, MISTRAL_PROVIDER_ID};

/// Current high-value subset of Pi's Mistral catalog.
///
/// The aliases are intentionally included because Mistral keeps those stable
/// while rotating the concrete deployment behind them.
pub fn mistral_models() -> Vec<ModelSpec> {
    vec![
        model(
            "codestral-latest",
            "Codestral (latest)",
            false,
            false,
            256_000,
            4_096,
            (0.3, 0.9, 0.03),
        ),
        model(
            "devstral-latest",
            "Devstral 2",
            false,
            false,
            262_144,
            262_144,
            (0.4, 2.0, 0.04),
        ),
        model(
            "magistral-medium-latest",
            "Magistral Medium (latest)",
            true,
            false,
            128_000,
            16_384,
            (2.0, 5.0, 0.2),
        ),
        model(
            "mistral-large-latest",
            "Mistral Large (latest)",
            false,
            true,
            262_144,
            262_144,
            (0.5, 1.5, 0.05),
        ),
        model(
            "mistral-medium-latest",
            "Mistral Medium (latest)",
            true,
            true,
            262_144,
            262_144,
            (1.5, 7.5, 0.15),
        ),
        model(
            "mistral-small-latest",
            "Mistral Small (latest)",
            true,
            true,
            256_000,
            256_000,
            (0.15, 0.6, 0.015),
        ),
        model(
            "pixtral-large-latest",
            "Pixtral Large (latest)",
            false,
            true,
            128_000,
            128_000,
            (2.0, 6.0, 0.2),
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
    cost: (f64, f64, f64),
) -> ModelSpec {
    let mut model = ModelSpec::new(MISTRAL_PROVIDER_ID, id, name, MISTRAL_CONVERSATIONS_API);
    model.base_url = Some(MISTRAL_BASE_URL.to_string());
    model.reasoning = reasoning;
    if image {
        model.input.push(ModelInput::Image);
    }
    model.cost = ModelCost {
        input: cost.0,
        output: cost.1,
        cache_read: cost.2,
        cache_write: 0.0,
        tiers: Vec::new(),
    };
    model.context_window = context_window;
    model.max_tokens = max_tokens;
    model
}
