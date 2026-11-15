use pi_core::ModelSpec;

/// Generated from the current checked-in Pi catalog with scripts/sync-provider-catalogs.py.
pub fn azure_openai_models() -> Vec<ModelSpec> {
    let mut models: Vec<ModelSpec> = serde_json::from_str(include_str!("data/azure.json"))
        .expect("checked-in Azure chat catalog");
    for model in &mut models {
        model.provider = crate::AZURE_PROVIDER_ID.into();
    }
    models
}
