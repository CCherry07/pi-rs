//! Presentation-neutral UI capabilities exposed to plugins.

use async_trait::async_trait;

use super::types::unbound;
use super::{NoticeLevel, PluginContextHandle, PluginContextResult, PresentationMode};

/// One row in a frontend-owned searchable multi-selection surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiMultiSelectOption {
    pub label: String,
    pub search_text: String,
    pub category: Option<String>,
    pub detail_lines: Vec<String>,
    pub read_only: bool,
    /// Lexicographically sortable keys for each configured sort mode.
    ///
    /// Keys are compared in order, so frontends can reproduce stable
    /// multi-column ordering without understanding plugin-specific fields.
    pub sort_values: Vec<Vec<String>>,
}

/// One action exposed by a frontend-owned multi-selection surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiMultiSelectAction {
    pub id: String,
    pub key: char,
    pub label: String,
    pub enabled: bool,
    /// Optional in-panel confirmation text. `{count}`, `{plural}`, and
    /// `{read_only_note}` are expanded from the current selection.
    pub confirmation: Option<String>,
}

/// Presentation-neutral configuration for a searchable multi-selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiMultiSelectRequest {
    pub title: String,
    pub options: Vec<UiMultiSelectOption>,
    pub actions: Vec<UiMultiSelectAction>,
    /// `(id, label)` pairs used by the optional category filter panel.
    pub categories: Vec<(String, String)>,
    /// `(label, descending)` pairs matching each option's `sort_values`.
    pub sort_modes: Vec<(String, bool)>,
    pub initially_selected: Vec<usize>,
    pub initial_query: String,
    pub initial_active_categories: Vec<String>,
    pub initial_sort_mode: usize,
    pub summary_lines: Vec<String>,
}

/// Semantic action returned by a searchable multi-selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiMultiSelectResponse {
    pub selected: Vec<usize>,
    pub action_id: String,
    pub query: String,
    pub active_categories: Vec<String>,
    pub sort_mode: usize,
}

/// Product-presentation access implemented by the owning product layer.
#[doc(hidden)]
#[async_trait]
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

    async fn ui_confirm(&self, _title: String, _message: String) -> PluginContextResult<bool> {
        unbound()
    }

    async fn ui_select(
        &self,
        _title: String,
        _options: Vec<String>,
    ) -> PluginContextResult<Option<usize>> {
        unbound()
    }

    async fn ui_multi_select(
        &self,
        _request: UiMultiSelectRequest,
    ) -> PluginContextResult<Option<UiMultiSelectResponse>> {
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

    /// Requests a binary decision from the active product UI.
    ///
    /// Presentation and input handling remain owned by the frontend; plugins
    /// receive only the semantic result.
    pub async fn confirm(
        &self,
        title: impl Into<String>,
        message: impl Into<String>,
    ) -> PluginContextResult<bool> {
        let access = self.handle.access()?;
        access.ui_confirm(title.into(), message.into()).await
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
        access.ui_select(title.into(), options).await
    }

    /// Requests a searchable multi-selection and one semantic batch action.
    ///
    /// Search, sorting, filtering, shortcuts, and rendering remain owned by
    /// the frontend. `None` means the user dismissed the surface.
    pub async fn multi_select(
        &self,
        request: UiMultiSelectRequest,
    ) -> PluginContextResult<Option<UiMultiSelectResponse>> {
        self.handle.access()?.ui_multi_select(request).await
    }
}
