use pi_plugin::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Default)]
pub struct FixtureAgentPlugin {
    started: AtomicBool,
}

#[pi_plugin::native_plugin]
impl Plugin for FixtureAgentPlugin {
    async fn session_start(
        &self,
        _: &SessionPluginContext,
        _: &SessionStartEvent,
    ) -> std::result::Result<(), PluginError> {
        self.started.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn before_agent_start(
        &self,
        _: AgentPluginContext,
        _: BeforeAgentStartEvent,
    ) -> std::result::Result<BeforeAgentStartPatch, PluginError> {
        Ok(BeforeAgentStartPatch {
            system_prompt: Some(format!("started={}", self.started.load(Ordering::SeqCst))),
            ..BeforeAgentStartPatch::default()
        })
    }

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
