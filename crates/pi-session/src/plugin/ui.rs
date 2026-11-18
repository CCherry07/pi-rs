use super::context::{PiPluginContext, context_failed};
use async_trait::async_trait;
use pi_plugin::{
    NoticeLevel, PluginContextError, PresentationMode, UiContextAccess, UiDialogRequest,
    UiDialogResponse, UiNotification,
};

#[async_trait]
impl UiContextAccess for PiPluginContext {
    fn ui_retire(&self) {
        self.ui_retired.send_replace(true);
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
        Ok(self.ui_bridge.as_ref().is_some_and(|bridge| {
            matches!(self.mode, PresentationMode::Tui)
                || (matches!(self.mode, PresentationMode::Rpc) && bridge.supports_confirm())
        }))
    }

    fn has_confirm(&self) -> Result<bool, PluginContextError> {
        Ok(self
            .ui_bridge
            .as_ref()
            .is_some_and(|bridge| bridge.supports_confirm()))
    }

    async fn ui_confirm(
        &self,
        title: String,
        message: String,
    ) -> Result<Option<bool>, PluginContextError> {
        Ok(self
            .ui_dialog(UiDialogRequest::Confirm { title, message }, None)
            .await?
            .map(|answer| matches!(answer, UiDialogResponse::Confirmed(true))))
    }

    async fn ui_dialog(
        &self,
        request: UiDialogRequest,
        timeout_ms: Option<u64>,
    ) -> Result<Option<UiDialogResponse>, PluginContextError> {
        let mut retired = self.ui_retired.subscribe();
        if *retired.borrow() {
            return Err(PluginContextError::Retired);
        }
        let Some(bridge) = &self.ui_bridge else {
            return Ok(None);
        };
        let answer = tokio::select! {
            biased;
            _ = retired.changed() => Err(PluginContextError::Retired),
            answer = bridge.dialog(request, timeout_ms) => answer.map_err(context_failed),
        };
        // Retirement can race with a ready bridge response after select! has
        // polled its first branch. Never expose that response to a stale plugin.
        if *retired.borrow() {
            return Err(PluginContextError::Retired);
        }
        answer
    }

    fn ui_notify(&self, level: NoticeLevel, message: String) -> Result<(), PluginContextError> {
        self.ui_notification(UiNotification::Notify {
            level: Some(level),
            message,
        })
    }

    fn ui_notification(&self, notification: UiNotification) -> Result<(), PluginContextError> {
        if let Some(bridge) = &self.ui_bridge
            && bridge
                .notification(notification.clone())
                .map_err(context_failed)?
        {
            return Ok(());
        }
        if let UiNotification::Notify { message, level } = notification {
            self.session()?
                .notify_plugin(message, level.unwrap_or(NoticeLevel::Info));
            return Ok(());
        }
        Err(PluginContextError::Unavailable(
            "semantic presentation notifications unavailable".into(),
        ))
    }
}
