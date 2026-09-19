use pi_plugin::prelude::*;

#[derive(Default)]
pub struct FixtureSessionPlugin;

#[pi_plugin::native_plugin]
impl Plugin for FixtureSessionPlugin {
    async fn session_start(
        &self,
        _context: &SessionPluginContext,
        _event: &SessionStartEvent,
    ) -> std::result::Result<(), PluginError> {
        Ok(())
    }
}
