use pi_plugin_sdk::provider::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct FixtureOptions {
    marker: Option<String>,
}

pub struct FixtureProviderPlugin {
    _marker: Option<String>,
}

impl NativePluginFactory for FixtureProviderPlugin {
    type Options = FixtureOptions;

    fn load(
        _context: &PluginLoadContext,
        options: Self::Options,
    ) -> PluginLoadResult<Self> {
        Ok(Self {
            _marker: options.marker,
        })
    }
}

#[pi_plugin_sdk::provider(factory)]
impl ProviderPlugin for FixtureProviderPlugin {}
