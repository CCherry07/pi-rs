use pi_plugin_sdk::agent::prelude::*;

#[derive(Default)]
pub struct FixtureAgentPlugin;

#[pi_plugin_sdk::agent]
impl AgentPlugin for FixtureAgentPlugin {
    async fn input(
        &self,
        context: InputContext,
        event: InputEvent,
    ) -> std::result::Result<InputPatch, PluginError> {
        let _mode = context.ui.mode()?;
        Ok(InputPatch::Transform {
            text: format!("{}-native", event.text),
            images: event.images,
        })
    }
}
