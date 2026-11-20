use std::sync::Arc;

use pi_core::{ModelId, ProviderId};
use pi_plugin::RegistriesBuilder;
use pi_plugin_models::{ModelsPlugin, ModelsPluginOptions};
use serde_json::{Value, json};

fn catalog(model: Value, overrides: Value) -> Result<Value, String> {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("models.json");
    std::fs::write(
        &path,
        json!({"providers":{"fixture":{
            "baseUrl":"http://127.0.0.1:9", "api":"openai-completions",
            "models":[model], "modelOverrides":{"model":overrides}
        }}})
        .to_string(),
    )
    .unwrap();
    let plugin = ModelsPlugin::load(ModelsPluginOptions::new(&path)).map_err(|e| e.to_string())?;
    let (_, _, registries) = RegistriesBuilder::new()
        .register_plugin_sets(Vec::new(), vec![Arc::new(plugin)])
        .map_err(|e| e.to_string())?;
    Ok(serde_json::to_value(
        registries
            .model(&ProviderId::new("fixture"), &ModelId::new("model"))
            .unwrap(),
    )
    .unwrap())
}

#[test]
fn catalog_keeps_and_merges_cache_and_thinking_sampling_metadata() {
    let model = catalog(
        json!({"id":"model", "promptCache":{"short":42.5,"long":300.0},
            "samplingParamsByThinkingLevel":{"off":{"temperature":0,"enabled":false},"high":{"temperature":0.2,"stop":""}}}),
        json!({"promptCache":{"long":600.0},"samplingParamsByThinkingLevel":{"high":{"temperature":0.7},"max":{}}}),
    ).unwrap();
    assert_eq!(model["promptCache"], json!({"short":42.5,"long":600.0}));
    assert_eq!(
        model["samplingParamsByThinkingLevel"],
        json!({
            "off":{"temperature":0,"enabled":false},"high":{"temperature":0.7,"stop":""},"max":{}
        })
    );
    for field in ["promptCache", "samplingParamsByThinkingLevel"] {
        assert!(
            catalog(json!({"id":"model"}), json!({}))
                .unwrap()
                .get(field)
                .is_none()
        );
        assert_eq!(
            catalog(json!({"id":"model",field:{}}), json!({})).unwrap()[field],
            json!({})
        );
        assert_eq!(
            catalog(json!({"id":"model"}), json!({field:{}})).unwrap()[field],
            json!({})
        );
    }
}

#[test]
fn catalog_rejects_nonpositive_prompt_cache_lifetimes() {
    for value in [0.0, -1.0] {
        assert!(
            catalog(
                json!({"id":"model","promptCache":{"short":value}}),
                json!({})
            )
            .is_err()
        );
        assert!(catalog(json!({"id":"model"}), json!({"promptCache":{"long":value}})).is_err());
    }
}
