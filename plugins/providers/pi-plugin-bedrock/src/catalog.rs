use pi_core::ModelSpec;

/// Generated from the current checked-in Pi catalog with scripts/sync-provider-catalogs.py.
pub fn amazon_bedrock_models() -> Vec<ModelSpec> {
    serde_json::from_str(include_str!("data/amazon-bedrock.json"))
        .expect("checked-in amazon-bedrock chat catalog")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BEDROCK_CONVERSE_STREAM_API;

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
