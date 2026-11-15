use pi_core::ModelSpec;

/// Generated from the current checked-in Pi catalog with scripts/sync-provider-catalogs.py.
pub fn github_copilot_models() -> Vec<ModelSpec> {
    serde_json::from_str(include_str!("data/github-copilot.json"))
        .expect("checked-in github-copilot chat catalog")
}

#[cfg(test)]
mod tests {
    use super::*;
    const ANTHROPIC_MESSAGES_API: &str = "anthropic-messages";
    const OPENAI_COMPLETIONS_API: &str = "openai-completions";
    const OPENAI_RESPONSES_API: &str = "openai-responses";

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
        assert!(
            models
                .iter()
                .any(|model| model.id.as_str() == "gpt-5.6-sol")
        );
    }
}
