use pi_core::ModelSpec;

/// Generated from the current checked-in Pi catalog with scripts/sync-provider-catalogs.py.
pub fn openrouter_models() -> Vec<ModelSpec> {
    serde_json::from_str(include_str!("data/openrouter.json"))
        .expect("checked-in openrouter chat catalog")
}
