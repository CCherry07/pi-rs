//! Presentation-neutral UI capabilities exposed to plugins.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::types::unbound;
use super::{NoticeLevel, PluginContextHandle, PluginContextResult, PresentationMode};

/// A presentation-neutral dialog that a terminal or graphical frontend can answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "camelCase")]
pub enum UiDialogRequest {
    Confirm { title: String, message: String },
    Select { title: String, options: Vec<String> },
    Input { title: String, placeholder: String },
    Editor { title: String, prefill: String },
}

/// A completed semantic dialog. Cancellation differs from an unsupported capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub enum UiDialogResponse {
    Confirmed(bool),
    Value(String),
    Cancelled,
}

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

    /// Answers a semantic dialog. `None` permits a declarative UI fallback;
    /// `Some(Cancelled)` completes the request without opening another dialog.
    async fn ui_dialog(
        &self,
        request: UiDialogRequest,
        _timeout_ms: Option<u64>,
    ) -> PluginContextResult<Option<UiDialogResponse>> {
        if let UiDialogRequest::Confirm { title, message } = request
            && self.has_confirm()?
        {
            return Ok(self
                .ui_confirm(title, message)
                .await?
                .map(UiDialogResponse::Confirmed));
        }
        Ok(None)
    }

    /// Answers a binary question through a semantic frontend prompt.
    ///
    /// `Ok(None)` means this context has no semantic confirmation surface and
    /// the request falls back to the declarative selector. Graphical
    /// frontends that do not render plugin surfaces implement this instead.
    async fn ui_confirm(
        &self,
        _title: String,
        _message: String,
    ) -> PluginContextResult<Option<bool>> {
        Ok(None)
    }

    /// True when `ui_confirm` reaches a person even though the declarative
    /// surface protocol is unavailable.
    fn has_confirm(&self) -> PluginContextResult<bool> {
        Ok(false)
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

    /// True when `confirm` can reach a person in this presentation mode.
    pub fn can_confirm(&self) -> PluginContextResult<bool> {
        let access = self.handle.access()?;
        Ok(access.has_ui()? || access.has_confirm()?)
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
        let title = title.into();
        let message = message.into();
        let access = self.handle.access()?;
        if let Some(answer) = access
            .ui_dialog(
                UiDialogRequest::Confirm {
                    title: title.clone(),
                    message: message.clone(),
                },
                None,
            )
            .await?
        {
            return Ok(matches!(answer, UiDialogResponse::Confirmed(true)));
        }
        Ok(self
            .select(
                format!("{title}\n{message}"),
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
        let access = self.handle.access()?;
        if options.is_empty() {
            return Ok(None);
        }
        let title = title.into();
        if let Some(answer) = access
            .ui_dialog(
                UiDialogRequest::Select {
                    title: title.clone(),
                    options: options.clone(),
                },
                None,
            )
            .await?
        {
            return Ok(match answer {
                UiDialogResponse::Value(value) => {
                    options.iter().position(|option| option == &value)
                }
                _ => None,
            });
        }
        let (_, signal) = crate::AbortHandle::new();
        self.custom(crate::ui::Selection { title, options }, signal)
            .await
    }

    /// Requests one line of text using the shared Rust-owned editor state.
    pub async fn input(
        &self,
        title: impl Into<String>,
        placeholder: impl Into<String>,
    ) -> PluginContextResult<Option<String>> {
        let title = title.into();
        let placeholder = placeholder.into();
        if let Some(answer) = self
            .handle
            .access()?
            .ui_dialog(
                UiDialogRequest::Input {
                    title: title.clone(),
                    placeholder: placeholder.clone(),
                },
                None,
            )
            .await?
        {
            return Ok(match answer {
                UiDialogResponse::Value(value) => Some(value),
                _ => None,
            });
        }
        let (_, signal) = crate::AbortHandle::new();
        self.custom(
            crate::ui::TextEntry {
                title,
                prefill: String::new(),
                placeholder,
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
        let title = title.into();
        let prefill = prefill.into();
        if let Some(answer) = self
            .handle
            .access()?
            .ui_dialog(
                UiDialogRequest::Editor {
                    title: title.clone(),
                    prefill: prefill.clone(),
                },
                None,
            )
            .await?
        {
            return Ok(match answer {
                UiDialogResponse::Value(value) => Some(value),
                _ => None,
            });
        }
        let (_, signal) = crate::AbortHandle::new();
        self.custom(
            crate::ui::TextEntry {
                title,
                prefill,
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
