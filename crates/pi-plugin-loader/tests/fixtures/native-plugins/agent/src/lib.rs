use pi_plugin_sdk::agent::prelude::*;

#[derive(Default)]
pub struct FixtureAgentPlugin;

#[pi_plugin_sdk::agent]
impl AgentPlugin for FixtureAgentPlugin {
    async fn input(
        &self,
        _context: InputContext,
        event: InputEvent,
    ) -> std::result::Result<InputPatch, PluginError> {
        Ok(InputPatch::Transform {
            text: format!("{}-native", event.text),
            images: event.images,
        })
    }
}
