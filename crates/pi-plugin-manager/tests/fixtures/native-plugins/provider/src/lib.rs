use pi_plugin::prelude::*;
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

impl PluginFactory for FixtureProviderPlugin {
    type Options = FixtureOptions;

    fn prepare(_context: &PrepareContext, options: Self::Options) -> PrepareResult<Option<Self>> {
        Ok(Some(Self {
            _marker: options.marker,
        }))
    }
}

#[pi_plugin::native_provider(factory)]
impl ProviderPlugin for FixtureProviderPlugin {}
