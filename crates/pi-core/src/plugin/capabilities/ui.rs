//! Presentation-neutral UI capabilities exposed to plugins.

use super::types::unbound;
use super::{NoticeLevel, PluginContextHandle, PluginContextResult, PresentationMode};

/// Product-presentation access implemented by the owning product layer.
#[doc(hidden)]
pub trait UiContextAccess: Send + Sync {
    fn mode(&self) -> PluginContextResult<PresentationMode> {
        unbound()
    }

    fn has_ui(&self) -> PluginContextResult<bool> {
        unbound()
    }

    fn ui_notify(&self, _level: NoticeLevel, _message: String) -> PluginContextResult<()> {
        unbound()
    }
}

/// Product presentation capabilities. Terminal ownership remains in the app.
#[derive(Clone)]
pub struct UiContext {
    handle: PluginContextHandle,
}

impl UiContext {
    pub(super) fn new(handle: PluginContextHandle) -> Self {
        Self { handle }
    }

    pub fn mode(&self) -> PluginContextResult<PresentationMode> {
        self.handle.access()?.mode()
    }

    pub fn is_available(&self) -> PluginContextResult<bool> {
        self.handle.access()?.has_ui()
    }

    pub fn notify(
        &self,
        level: NoticeLevel,
        message: impl Into<String>,
    ) -> PluginContextResult<()> {
        self.handle.access()?.ui_notify(level, message.into())
    }
}
