use pi_plugin::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct FixtureOptions {
    marker: Option<String>,
    expected_agent_dir: Option<std::path::PathBuf>,
    expected_project_trusted: Option<bool>,
}

pub struct FixtureProviderPlugin {
    _marker: Option<String>,
}

#[pi_plugin::native_provider(factory)]
impl ProviderPlugin for FixtureProviderPlugin {
    type Options = FixtureOptions;

    fn prepare(context: &PrepareContext, options: Self::Options) -> PrepareResult<Option<Self>> {
        if let Some(expected) = &options.expected_agent_dir
            && context.agent_dir() != expected
        {
            return Err(PrepareError::initialization(
                "incorrect agent profile directory",
            ));
        }
        if let Some(expected) = options.expected_project_trusted
            && context.project_trusted() != expected
        {
            return Err(PrepareError::initialization("incorrect project trust"));
        }
        Ok(Some(Self {
            _marker: options.marker,
        }))
    }
}
