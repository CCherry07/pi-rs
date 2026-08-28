use pi_plugin_sdk::session::prelude::*;

#[derive(Default)]
pub struct FixtureSessionPlugin;

#[pi_plugin_sdk::session]
impl SessionPlugin for FixtureSessionPlugin {
    async fn session_start(
        &self,
        _context: &SessionPluginContext,
        _event: &SessionStartEvent,
    ) -> Result<(), SessionPluginError> {
        Ok(())
    }
}
