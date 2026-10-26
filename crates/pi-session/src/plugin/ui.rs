use super::context::{PiPluginContext, context_failed};
use async_trait::async_trait;
use pi_plugin::{NoticeLevel, PluginContextError, PresentationMode, UiContextAccess};

#[async_trait]
impl UiContextAccess for PiPluginContext {
    fn ui_retire(&self) {
        if let Some(bridge) = &self.ui_bridge {
            for owner in self
                .ui_owners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .drain()
            {
                let _ = bridge.ui_command(
                    owner,
                    pi_plugin::UiCommand::Surface(pi_plugin::UiSurfaceCommand::Close),
                );
            }
        }
    }

    fn ui_surface_snapshot(&self) -> Result<pi_plugin::UiSurfaceSnapshot, PluginContextError> {
        self.ui_bridge
            .as_ref()
            .ok_or_else(|| {
                PluginContextError::Unavailable("extension surfaces unavailable".into())
            })?
            .surface_snapshot()
            .map_err(context_failed)
    }
    fn ui_command(
        &self,
        owner: String,
        command: pi_plugin::UiCommand,
    ) -> Result<(), PluginContextError> {
        self.ui_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(owner.clone());
        let closes_surface = matches!(
            command,
            pi_plugin::UiCommand::Surface(pi_plugin::UiSurfaceCommand::Close)
        );
        self.ui_bridge
            .as_ref()
            .ok_or_else(|| {
                PluginContextError::Unavailable("extension surfaces unavailable".into())
            })?
            .ui_command(owner.clone(), command)
            .map_err(context_failed)?;
        if closes_surface {
            self.ui_owners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&owner);
        }
        Ok(())
    }
    async fn ui_surface_exchange(
        &self,
        frame: pi_plugin::UiSurfaceFrame,
    ) -> Result<pi_plugin::UiSurfaceEvent, PluginContextError> {
        if !matches!(self.mode, PresentationMode::Tui) {
            return Ok(pi_plugin::UiSurfaceEvent::Closed);
        }
        self.ui_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(frame.owner.clone());
        self.ui_bridge
            .as_ref()
            .ok_or_else(|| {
                PluginContextError::Unavailable("extension surfaces unavailable".into())
            })?
            .surface_exchange(frame)
            .await
            .map_err(context_failed)
    }

    fn mode(&self) -> Result<PresentationMode, PluginContextError> {
        Ok(self.mode)
    }

    fn has_ui(&self) -> Result<bool, PluginContextError> {
        Ok(matches!(self.mode, PresentationMode::Tui) && self.ui_bridge.is_some())
    }

    fn ui_notify(&self, level: NoticeLevel, message: String) -> Result<(), PluginContextError> {
        self.session()?.notify_plugin(message, level);
        Ok(())
    }
}
