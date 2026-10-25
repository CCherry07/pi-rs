//! Declarative plugin UI. The frontend owns layout, editing, focus and terminal rendering.
use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// Stable identity plus semantic content. IDs retain frontend state across document revisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiNode {
    pub id: String,
    #[serde(flatten)]
    pub kind: UiNodeKind,
}

impl UiNode {
    pub fn new(id: impl Into<String>, kind: UiNodeKind) -> Self {
        Self {
            id: id.into(),
            kind,
        }
    }

    /// Rejects ambiguous state identities before a tree reaches a frontend.
    ///
    /// Node identities are scoped to one region or transcript tree. Every node requires a
    /// nonempty ID, including presentation-only nodes that may later become interactive.
    pub fn validate(&self) -> Result<(), String> {
        let mut pending = vec![self];
        let mut ids = HashSet::new();
        while let Some(node) = pending.pop() {
            if node.id.trim().is_empty() {
                return Err("UI node ID must not be empty".into());
            }
            if !ids.insert(node.id.as_str()) {
                return Err(format!("duplicate UI node ID: {}", node.id));
            }
            match &node.kind {
                UiNodeKind::Column { children, .. } | UiNodeKind::Row { children, .. } => {
                    pending.extend(children.iter().rev());
                }
                UiNodeKind::Panel { child, .. } | UiNodeKind::Scroll { child, .. } => {
                    pending.push(child)
                }
                UiNodeKind::MultiSelect { request } => request.validate()?,
                _ => {}
            }
        }
        Ok(())
    }

    pub fn text(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self::new(
            id,
            UiNodeKind::Text {
                text: text.into(),
                tone: UiTone::Default,
            },
        )
    }
}

/// Shared components implemented by the Rust frontend, never by the plugin VM.
///
/// Initial values seed a new node. Publishing the same ID and kind preserves frontend-owned
/// editing/selection state; changing the ID intentionally mounts fresh state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum UiNodeKind {
    Text {
        text: String,
        #[serde(default)]
        tone: UiTone,
    },
    /// Plugin-drawn content in a frontend-owned region.
    /// Frontends parse styling and clip these rows; they never write them as terminal commands.
    Custom {
        lines: Vec<String>,
        #[serde(default)]
        interactive: bool,
        #[serde(default)]
        key_release: bool,
        /// Component-local (row, column), in terminal cells.
        #[serde(default)]
        cursor: Option<(u16, u16)>,
    },
    Markdown {
        text: String,
    },
    Link {
        text: String,
        url: String,
    },
    Column {
        children: Vec<UiNode>,
        #[serde(default)]
        gap: u16,
    },
    Row {
        children: Vec<UiNode>,
        #[serde(default)]
        gap: u16,
    },
    Panel {
        #[serde(default)]
        title: String,
        child: Box<UiNode>,
    },
    Scroll {
        child: Box<UiNode>,
        height: u16,
    },
    Spacer {
        height: u16,
    },
    Select {
        options: Vec<String>,
        #[serde(default)]
        initial_selected: usize,
    },
    /// A frontend-owned searchable, filterable and sortable multi-selector.
    /// Initial values seed a new node; the frontend retains live state while ID and kind survive.
    MultiSelect {
        request: Box<UiMultiSelectRequest>,
    },
    Input {
        #[serde(default)]
        initial_value: String,
        #[serde(default)]
        placeholder: String,
        #[serde(default)]
        completions: Vec<UiCompletion>,
    },
    Editor {
        #[serde(default)]
        initial_value: String,
        #[serde(default)]
        placeholder: String,
        height: u16,
        #[serde(default)]
        completions: Vec<UiCompletion>,
    },
    Button {
        text: String,
    },
    Progress {
        text: String,
    },
    Image {
        /// Base64-encoded image content. Terminal escape sequences are never part of this API.
        data: String,
        mime_type: String,
        #[serde(default)]
        alt: String,
        width: u16,
        height: u16,
    },
}

/// Candidate content for the frontend-owned input/editor completion menu.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiCompletion {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

/// One row in the shared searchable multi-selection control.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiMultiSelectOption {
    pub label: String,
    pub search_text: String,
    pub category: Option<String>,
    pub detail_lines: Vec<String>,
    pub read_only: bool,
    /// Lexicographically sortable keys for each configured sort mode.
    pub sort_values: Vec<Vec<String>>,
}

/// One action exposed by the shared searchable multi-selection control.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiMultiSelectAction {
    pub id: String,
    pub key: char,
    pub label: String,
    pub enabled: bool,
    /// `{count}`, `{plural}`, and `{read_only_note}` are expanded from the selection.
    pub confirmation: Option<String>,
}

/// Presentation-neutral configuration for the shared searchable multi-selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiMultiSelectRequest {
    pub title: String,
    pub options: Vec<UiMultiSelectOption>,
    pub actions: Vec<UiMultiSelectAction>,
    pub categories: Vec<(String, String)>,
    /// `(label, descending)` pairs matching each option's `sort_values`.
    pub sort_modes: Vec<(String, bool)>,
    pub initially_selected: Vec<usize>,
    pub initial_query: String,
    pub initial_active_categories: Vec<String>,
    pub initial_sort_mode: usize,
    pub summary_lines: Vec<String>,
}

impl UiMultiSelectRequest {
    fn validate(&self) -> Result<(), String> {
        let mut categories = HashSet::new();
        for (id, _) in &self.categories {
            if id.trim().is_empty() || !categories.insert(id) {
                return Err(format!("invalid or duplicate multi-select category: {id}"));
            }
        }
        if self
            .initial_active_categories
            .iter()
            .any(|category| !categories.contains(category))
        {
            return Err("multi-select initial category is not registered".into());
        }
        if self.options.iter().any(|option| {
            option
                .category
                .as_ref()
                .is_some_and(|category| !categories.contains(category))
        }) {
            return Err("multi-select option category is not registered".into());
        }
        if self
            .initially_selected
            .iter()
            .any(|index| *index >= self.options.len())
        {
            return Err("multi-select initial selection is out of range".into());
        }
        if self.sort_modes.is_empty() {
            if self.initial_sort_mode != 0 {
                return Err("multi-select initial sort mode is out of range".into());
            }
        } else if self.initial_sort_mode >= self.sort_modes.len() {
            return Err("multi-select initial sort mode is out of range".into());
        }
        let mut action_ids = HashSet::new();
        let mut action_keys = HashSet::new();
        for action in &self.actions {
            if action.id.trim().is_empty() || !action_ids.insert(&action.id) {
                return Err(format!(
                    "invalid or duplicate multi-select action: {}",
                    action.id
                ));
            }
            if !action_keys.insert(action.key) {
                return Err(format!("duplicate multi-select action key: {}", action.key));
            }
            if matches!(action.key, ' ' | '/' | 'a' | 'n' | 's' | 'f') {
                return Err(format!(
                    "multi-select action key is reserved by the control: {}",
                    action.key
                ));
            }
        }
        Ok(())
    }
}

/// Semantic result returned by the shared searchable multi-selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiMultiSelectResponse {
    pub selected: Vec<usize>,
    pub action_id: String,
    pub query: String,
    pub active_categories: Vec<String>,
    pub sort_mode: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UiTone {
    #[default]
    Default,
    Accent,
    Muted,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UiRegionPlacement {
    Header,
    AboveEditor,
    BelowEditor,
    Footer,
    Editor,
    Overlay,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiRegion {
    pub id: String,
    pub placement: UiRegionPlacement,
    pub root: UiNode,
    #[serde(default)]
    pub overlay: Option<UiOverlayOptions>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiOverlayOptions {
    pub width: Option<u16>,
    pub max_height: Option<u16>,
    #[serde(default = "default_modal")]
    pub modal: bool,
}

fn default_modal() -> bool {
    true
}

impl Default for UiOverlayOptions {
    fn default() -> Self {
        Self {
            width: None,
            max_height: None,
            modal: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiShortcut {
    pub key: String,
}

/// Desired UI document. Only a newer revision replaces a live owner's document; an equal
/// revision may attach the next event waiter without resetting frontend state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiSurfaceFrame {
    pub owner: String,
    pub revision: u64,
    #[serde(default)]
    pub regions: Vec<UiRegion>,
    #[serde(default)]
    pub hide_working: bool,
    #[serde(default)]
    pub render_transcript: bool,
    #[serde(default)]
    pub transcript: Vec<UiTranscriptRender>,
    #[serde(default)]
    pub shortcuts: Vec<UiShortcut>,
}

impl UiSurfaceFrame {
    /// Checks the identities used to retain control state and route semantic events.
    /// No frontend state should change until this complete validation succeeds.
    pub fn validate(&self) -> Result<(), String> {
        if self.owner.trim().is_empty() {
            return Err("UI owner must not be empty".into());
        }
        let mut regions = HashSet::new();
        for region in &self.regions {
            if region.id.trim().is_empty() {
                return Err("UI region ID must not be empty".into());
            }
            if !regions.insert(region.id.as_str()) {
                return Err(format!("duplicate UI region ID: {}", region.id));
            }
            region
                .root
                .validate()
                .map_err(|error| format!("UI region {}: {error}", region.id))?;
        }
        let mut transcript = HashSet::new();
        for item in &self.transcript {
            if !transcript.insert(item.index) {
                return Err(format!("duplicate UI transcript index: {}", item.index));
            }
            item.root
                .validate()
                .map_err(|error| format!("UI transcript {}: {error}", item.index))?;
        }
        Ok(())
    }
}

/// A semantic transcript row and its content identity, used by presentation callbacks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiTranscriptItem {
    pub index: usize,
    pub fingerprint: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiTranscriptRender {
    pub index: usize,
    pub fingerprint: String,
    pub root: UiNode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiSurfaceSnapshot {
    pub columns: u16,
    pub rows: u16,
    pub editor_text: String,
    pub tools_expanded: bool,
    #[serde(default)]
    pub is_running: bool,
    #[serde(default)]
    pub transcript: Vec<UiTranscriptItem>,
    #[serde(default)]
    pub transcript_width: u16,
    #[serde(default)]
    pub areas: Vec<UiSurfaceArea>,
}

/// Read-only geometry of frontend-allocated slots; plugins do not return cell coordinates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiSurfaceArea {
    pub placement: UiRegionPlacement,
    pub row: u16,
    pub column: u16,
    pub width: u16,
    pub height: u16,
}
impl Default for UiSurfaceSnapshot {
    fn default() -> Self {
        Self {
            columns: 80,
            rows: 24,
            editor_text: String::new(),
            tools_expanded: false,
            is_running: false,
            transcript: Vec::new(),
            transcript_width: 0,
            areas: Vec::new(),
        }
    }
}

/// Semantic control events, emitted after Rust has updated its local control state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum UiInteraction {
    Change {
        value: serde_json::Value,
    },
    Submit {
        value: serde_json::Value,
    },
    Activate,
    Cancel,
    Shortcut {
        key: String,
    },
    /// Keyboard/paste input delivered only to a focused custom component.
    CustomInput {
        data: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum UiSurfaceEvent {
    HostResult {
        id: u64,
        result: Option<String>,
        error: Option<String>,
        snapshot: UiSurfaceSnapshot,
    },
    Refresh {
        snapshot: UiSurfaceSnapshot,
    },
    Interaction {
        region_id: String,
        node_id: String,
        event: UiInteraction,
        snapshot: UiSurfaceSnapshot,
        revision: u64,
    },
    Closed,
}

/// One transport envelope with explicit command categories. The frontend owns admission and
/// execution; the channel discriminant does not grant a new permission or callback capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "channel",
    content = "command",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub enum UiCommand {
    Surface(UiSurfaceCommand),
    Product(UiProductCommand),
    Host(UiHostRequest),
}

/// Lifecycle and focus commands interpreted entirely by the shared surface runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum UiSurfaceCommand {
    Wake,
    Close,
    Focus { region_id: String, node_id: String },
}

/// Product presentation commands executed outside the surface runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum UiProductCommand {
    AppAction {
        action: UiAppAction,
    },
    EditorText {
        text: String,
        append: bool,
    },
    ToolsExpanded {
        expanded: bool,
    },
    Title {
        title: String,
    },
    Theme {
        name: String,
    },
    Progress {
        active: bool,
    },
    ProgramStatus {
        state: String,
        text: Option<String>,
        progress: Option<u8>,
    },
    Submit {
        text: String,
    },
}

/// One correlated request for a physical terminal or operating-system operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiHostRequest {
    pub id: u64,
    pub operation: UiHostOperation,
}

/// Physical terminal/system operations are executed by the owning frontend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum UiHostOperation {
    Clipboard {
        text: String,
    },
    ExternalEditor {
        text: String,
        command: Option<String>,
    },
    Suspend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UiAppAction {
    Clear,
    Interrupt,
    Exit,
    Paste,
    Copy,
    FollowUp,
    Dequeue,
    NewSession,
    Tree,
    Fork,
    Resume,
}
