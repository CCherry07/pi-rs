use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{DeferredHandle, ModelId, ProviderId, Usage, UsageCost};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelInput {
    #[default]
    Text,
    Image,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tiers: Vec<ModelCostTier>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCostTier {
    pub input_tokens_above: u64,
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

impl ModelCost {
    /// Calculates Pi-compatible per-request cost from token usage.
    ///
    /// Pricing is expressed per million tokens. A tier replaces every rate
    /// for the whole request when total input usage is strictly greater than
    /// its threshold; the highest matching threshold wins. Anthropic's
    /// one-hour cache writes are charged at twice the selected input rate.
    pub fn calculate(&self, usage: &Usage) -> UsageCost {
        let total_input = usage
            .input
            .saturating_add(usage.cache_read)
            .saturating_add(usage.cache_write);
        let tier = self
            .tiers
            .iter()
            .filter(|tier| total_input > tier.input_tokens_above)
            .max_by_key(|tier| tier.input_tokens_above);
        let (input_rate, output_rate, cache_read_rate, cache_write_rate) = tier.map_or(
            (self.input, self.output, self.cache_read, self.cache_write),
            |tier| (tier.input, tier.output, tier.cache_read, tier.cache_write),
        );
        let long_cache_write = usage.cache_write_1h.unwrap_or(0).min(usage.cache_write);
        let short_cache_write = usage.cache_write.saturating_sub(long_cache_write);
        let per_million = 1_000_000.0;
        let input = input_rate * usage.input as f64 / per_million;
        let output = output_rate * usage.output as f64 / per_million;
        let cache_read = cache_read_rate * usage.cache_read as f64 / per_million;
        let cache_write = (cache_write_rate * short_cache_write as f64
            + input_rate * 2.0 * long_cache_write as f64)
            / per_million;
        UsageCost {
            input,
            output,
            cache_read,
            cache_write,
            total: input + output + cache_read + cache_write,
        }
    }
}

/// Immutable model metadata contributed by a provider/catalog plugin.
///
/// Routing credentials and resolved headers deliberately stay inside the
/// provider adapter; snapshots are safe to expose to selectors and sessions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSpec {
    pub provider: ProviderId,
    pub id: ModelId,
    pub name: String,
    pub api: String,
    pub base_url: Option<String>,
    pub reasoning: bool,
    pub input: Vec<ModelInput>,
    pub cost: ModelCost,
    pub context_window: u64,
    pub max_tokens: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub thinking_level_map: BTreeMap<String, Option<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sampling_params: BTreeMap<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compat: Option<Value>,
}

impl ModelSpec {
    pub fn new(
        provider: impl Into<ProviderId>,
        id: impl Into<ModelId>,
        name: impl Into<String>,
        api: impl Into<String>,
    ) -> Self {
        Self {
            provider: provider.into(),
            id: id.into(),
            name: name.into(),
            api: api.into(),
            base_url: None,
            reasoning: false,
            input: vec![ModelInput::Text],
            cost: ModelCost::default(),
            context_window: 128_000,
            max_tokens: 16_384,
            thinking_level_map: BTreeMap::new(),
            sampling_params: BTreeMap::new(),
            compat: None,
        }
    }
}

#[cfg(test)]
mod cost_tests {
    use super::*;

    fn cost() -> ModelCost {
        ModelCost {
            input: 1.0,
            output: 2.0,
            cache_read: 0.25,
            cache_write: 1.25,
            tiers: vec![
                ModelCostTier {
                    input_tokens_above: 100,
                    input: 3.0,
                    output: 4.0,
                    cache_read: 0.5,
                    cache_write: 3.75,
                },
                ModelCostTier {
                    input_tokens_above: 200,
                    input: 5.0,
                    output: 6.0,
                    cache_read: 1.0,
                    cache_write: 6.25,
                },
            ],
        }
    }

    #[test]
    fn cost_tiers_use_total_input_strictly_above_the_highest_threshold() {
        let exactly = Usage {
            input: 50,
            cache_read: 25,
            cache_write: 25,
            output: 10,
            ..Usage::default()
        };
        let above = Usage {
            input: 51,
            ..exactly.clone()
        };
        let highest = Usage {
            input: 151,
            ..exactly.clone()
        };

        assert_eq!(cost().calculate(&exactly).input, 0.000_05);
        assert_eq!(cost().calculate(&above).input, 0.000_153);
        assert_eq!(cost().calculate(&highest).input, 0.000_755);
    }

    #[test]
    fn one_hour_cache_writes_use_twice_the_selected_input_rate() {
        let usage = Usage {
            input: 100,
            output: 20,
            cache_read: 40,
            cache_write: 80,
            cache_write_1h: Some(30),
            ..Usage::default()
        };
        let calculated = cost().calculate(&usage);

        assert_eq!(calculated.input, 0.000_5);
        assert_eq!(calculated.output, 0.000_12);
        assert_eq!(calculated.cache_read, 0.000_04);
        assert_eq!(calculated.cache_write, 0.000_612_5);
        assert_eq!(calculated.total, 0.001_272_5);
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StopReason {
    #[default]
    Stop,
    Pending,
    Length,
    ToolUse,
    Error,
    Aborted,
    Deferred,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    #[default]
    Off,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl ThinkingLevel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }
}

/// Optional current-settings overrides for token-budget based reasoning APIs.
/// Providers with native effort levels may ignore these values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingBudgets {
    pub minimal: Option<u64>,
    pub low: Option<u64>,
    pub medium: Option<u64>,
    pub high: Option<u64>,
}

impl ThinkingBudgets {
    pub const fn for_level(self, level: ThinkingLevel) -> Option<u64> {
        match level {
            ThinkingLevel::Off => None,
            ThinkingLevel::Minimal => self.minimal,
            ThinkingLevel::Low => self.low,
            ThinkingLevel::Medium => self.medium,
            ThinkingLevel::High | ThinkingLevel::XHigh | ThinkingLevel::Max => self.high,
        }
    }
}

impl std::str::FromStr for ThinkingLevel {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "off" => Ok(Self::Off),
            "minimal" => Ok(Self::Minimal),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::XHigh),
            "max" => Ok(Self::Max),
            other => Err(format!("unknown thinking level: {other}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseMetadata {
    pub provider: ProviderId,
    pub model: ModelId,
    pub api: String,
    pub timestamp_ms: i64,
    pub response_model: Option<String>,
    pub response_id: Option<String>,
    pub diagnostics: Option<Vec<Value>>,
    pub deferred: Option<DeferredHandle>,
    pub raw_stop_reason: Option<String>,
    pub end_turn: Option<bool>,
}

impl ResponseMetadata {
    pub fn new(
        provider: ProviderId,
        model: ModelId,
        api: impl Into<String>,
        timestamp_ms: i64,
    ) -> Self {
        Self {
            provider,
            model,
            api: api.into(),
            timestamp_ms,
            response_model: None,
            response_id: None,
            diagnostics: None,
            deferred: None,
            raw_stop_reason: None,
            end_turn: None,
        }
    }
}
