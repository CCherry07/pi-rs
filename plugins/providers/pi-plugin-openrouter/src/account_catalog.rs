use pi_core::{ModelCost, ModelCostTier, ModelSpec};
use serde_json::Value;
use std::collections::BTreeMap;

/// Apply account-specific allowlists, limits, and per-token pricing to a catalog.
pub fn personalize_catalog(
    catalog: Vec<ModelSpec>,
    body: &Value,
) -> Result<Vec<ModelSpec>, String> {
    let data = body
        .get("data")
        .and_then(Value::as_array)
        .ok_or("invalid OpenRouter account catalog")?;
    let allowed: BTreeMap<_, _> = data
        .iter()
        .filter_map(|entry| {
            entry
                .get("id")
                .and_then(Value::as_str)
                .map(|id| (id, entry))
        })
        .collect();
    Ok(catalog
        .into_iter()
        .filter_map(|mut model| {
            let entry = allowed.get(model.id.as_str())?;
            let positive = |value: Option<&Value>| value.and_then(Value::as_u64).filter(|n| *n > 0);
            if let Some(limit) = positive(entry.pointer("/top_provider/context_length"))
                .or_else(|| positive(entry.get("context_length")))
            {
                model.context_window = limit;
            }
            if let Some(limit) = positive(entry.pointer("/top_provider/max_completion_tokens")) {
                model.max_tokens = limit;
            }
            if let Some(pricing) = entry.get("pricing") {
                model.cost = pricing_cost(pricing);
            }
            Some(model)
        })
        .collect())
}
fn rate(value: &Value, name: &str, fallback: f64) -> f64 {
    value
        .get(name)
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(|value| (value * 1e12).round() / 1e6)
        .unwrap_or(fallback)
}
fn pricing_cost(value: &Value) -> ModelCost {
    let mut cost = ModelCost {
        input: rate(value, "prompt", 0.0),
        output: rate(value, "completion", 0.0),
        cache_read: rate(value, "input_cache_read", 0.0),
        cache_write: rate(value, "input_cache_write", 0.0),
        tiers: Vec::new(),
    };
    if let Some(overrides) = value.get("overrides").and_then(Value::as_array) {
        for entry in overrides {
            if ["utc_start", "utc_end", "utc_days"]
                .iter()
                .any(|key| entry.get(key).is_some())
            {
                continue;
            }
            if let Some(threshold) = entry.get("min_prompt_tokens").and_then(Value::as_u64) {
                cost.tiers.push(ModelCostTier {
                    input_tokens_above: threshold,
                    input: rate(entry, "prompt", cost.input),
                    output: rate(entry, "completion", cost.output),
                    cache_read: rate(entry, "input_cache_read", cost.cache_read),
                    cache_write: rate(entry, "input_cache_write", cost.cache_write),
                });
            }
        }
    }
    cost
}

/// Refresh before publishing a generation. No registry is mutated by network I/O.
pub async fn refresh_account_catalog(
    base_url: &str,
    key: &str,
    headers: &BTreeMap<String, String>,
    proxy: Option<&str>,
    catalog: Vec<ModelSpec>,
) -> Result<Vec<ModelSpec>, String> {
    let mut client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(15));
    if let Some(proxy) = proxy {
        client = client.proxy(reqwest::Proxy::all(proxy).map_err(|error| error.to_string())?);
    }
    let mut request = client
        .build()
        .map_err(|error| error.to_string())?
        .get(format!("{}/models/user", base_url.trim_end_matches('/')))
        .bearer_auth(key)
        .header("accept", "application/json");
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("OpenRouter account catalog request failed: {error}"))?;
    if matches!(response.status().as_u16(), 401 | 403 | 404) {
        return Ok(catalog);
    }
    if !response.status().is_success() {
        return Err(format!(
            "OpenRouter account catalog: HTTP {}",
            response.status()
        ));
    }
    let body = response
        .json()
        .await
        .map_err(|_| "invalid OpenRouter account catalog JSON")?;
    personalize_catalog(catalog, &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn account_catalog_filters_models_and_applies_limits_and_price_tiers() {
        let models = vec![
            ModelSpec::new("openrouter", "allowed", "Allowed", "openai-completions"),
            ModelSpec::new("openrouter", "blocked", "Blocked", "openai-completions"),
        ];
        let body = serde_json::json!({"data":[{"id":"allowed","context_length":100000,"top_provider":{"max_completion_tokens":8192},"pricing":{"prompt":"0.000002","completion":"0.00001","overrides":[{"min_prompt_tokens":50000,"prompt":"0.000004"},{"min_prompt_tokens":0,"utc_start":0,"prompt":"1"}]}}]});
        let models = personalize_catalog(models, &body).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].context_window, 100000);
        assert_eq!(models[0].max_tokens, 8192);
        assert_eq!(models[0].cost.input, 2.0);
        assert_eq!(models[0].cost.tiers.len(), 1);
        assert_eq!(models[0].cost.tiers[0].input, 4.0);
        assert_eq!(models[0].cost.tiers[0].output, 10.0);
    }
}
