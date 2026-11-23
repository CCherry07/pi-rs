use pi_core::ModelSpec;
use serde_json::{Value, json};

#[test]
fn model_round_trip_preserves_prompt_cache_and_level_sampling_metadata() {
    // Current Pi ModelPromptCacheSchema and SamplingParamsByThinkingLevel:
    // empty objects are valid and distinct from omitted optional metadata.
    let base = serde_json::to_value(ModelSpec::new("fixture", "model", "Model", "test")).unwrap();
    for metadata in [
        json!({}),
        json!({"promptCache":{},"samplingParamsByThinkingLevel":{}}),
        json!({
            "promptCache":{"short":42.5,"long":3600.0},
            "samplingParamsByThinkingLevel":{
                "off":{"temperature":0,"stop":"","enabled":false,"custom":null},
                "high":{"temperature":0.25},
                "max":{}
            }
        }),
    ] {
        let mut input = base.clone();
        input
            .as_object_mut()
            .unwrap()
            .extend(metadata.as_object().unwrap().clone());
        let model: ModelSpec = serde_json::from_value(input.clone()).unwrap();
        let output: Value = serde_json::to_value(model).unwrap();
        assert_eq!(output, input);
    }
}
