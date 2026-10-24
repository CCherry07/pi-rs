//! Presentation-neutral UI capabilities exposed to plugins.

use async_trait::async_trait;

use super::types::unbound;
use super::{NoticeLevel, PluginContextHandle, PluginContextResult, PresentationMode};

/// Product-presentation access implemented by the owning product layer.
#[doc(hidden)]
#[async_trait]
pub trait UiContextAccess: Send + Sync {
    /// Releases frontend surfaces when this generation loses its capabilities.
    fn ui_retire(&self) {}

    fn ui_surface_snapshot(&self) -> PluginContextResult<super::UiSurfaceSnapshot> {
        unbound()
    }
    fn ui_command(&self, _owner: String, _command: super::UiCommand) -> PluginContextResult<()> {
        unbound()
    }
    async fn ui_surface_exchange(
        &self,
        _frame: super::UiSurfaceFrame,
    ) -> PluginContextResult<super::UiSurfaceEvent> {
        unbound()
    }

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

    /// Runs a Rust component through the same declarative document/event protocol as JavaScript UI.
    ///
    /// The future owns the component: completion, cancellation and dropping the future close
    /// its surface. Noninteractive modes return `None`. An expired generation returns `Retired`.
    /// Rendering and event callbacks must be short and nonblocking; use the component's waker
    /// for background updates. The frontend owns each node's layout, input and editing state.
    pub async fn custom<C: crate::UiComponent>(
        &self,
        component: C,
        signal: crate::AbortSignal,
    ) -> PluginContextResult<Option<C::Output>> {
        crate::ui::run_component(self.handle.clone(), component, signal).await
    }

    pub fn notify(
        &self,
        level: NoticeLevel,
        message: impl Into<String>,
    ) -> PluginContextResult<()> {
        self.handle.access()?.ui_notify(level, message.into())
    }

    /// Requests a binary decision from the active product UI.
    ///
    /// Presentation and input handling remain owned by the frontend; plugins
    /// receive only the semantic result.
    pub async fn confirm(
        &self,
        title: impl Into<String>,
        message: impl Into<String>,
    ) -> PluginContextResult<bool> {
        Ok(self
            .select(
                format!("{}\n{}", title.into(), message.into()),
                vec!["Yes".into(), "No".into()],
            )
            .await?
            == Some(0))
    }

    /// Requests one semantic choice from the active product UI.
    ///
    /// The returned index addresses the supplied option list. `None` means
    /// the user dismissed the selector.
    pub async fn select(
        &self,
        title: impl Into<String>,
        options: Vec<String>,
    ) -> PluginContextResult<Option<usize>> {
        self.handle.access()?;
        if options.is_empty() {
            return Ok(None);
        }
        let (_, signal) = crate::AbortHandle::new();
        self.custom(
            crate::ui::Selection {
                title: title.into(),
                options,
            },
            signal,
        )
        .await
    }

    /// Requests one line of text using the shared Rust-owned editor state.
    pub async fn input(
        &self,
        title: impl Into<String>,
        placeholder: impl Into<String>,
    ) -> PluginContextResult<Option<String>> {
        let (_, signal) = crate::AbortHandle::new();
        self.custom(
            crate::ui::TextEntry {
                title: title.into(),
                prefill: String::new(),
                placeholder: placeholder.into(),
                multiline: false,
            },
            signal,
        )
        .await
    }

    /// Requests multiline text. Enter inserts a newline and Ctrl+Enter submits.
    pub async fn editor(
        &self,
        title: impl Into<String>,
        prefill: impl Into<String>,
    ) -> PluginContextResult<Option<String>> {
        let (_, signal) = crate::AbortHandle::new();
        self.custom(
            crate::ui::TextEntry {
                title: title.into(),
                prefill: prefill.into(),
                placeholder: String::new(),
                multiline: true,
            },
            signal,
        )
        .await
    }

    /// Requests a searchable multi-selection and one semantic batch action.
    ///
    /// Search, sorting, filtering, shortcuts, and rendering remain owned by
    /// the frontend. `None` means the user dismissed the surface.
    pub async fn multi_select(
        &self,
        request: super::UiMultiSelectRequest,
    ) -> PluginContextResult<Option<super::UiMultiSelectResponse>> {
        let (_, signal) = crate::AbortHandle::new();
        self.custom(crate::ui::MultiSelection { request }, signal)
            .await
    }
}
