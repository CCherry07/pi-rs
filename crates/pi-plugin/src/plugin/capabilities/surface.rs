//! Preformatted extension surfaces. The frontend owns layout, input and terminal access.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiTextSpan {
    pub text: String,
    pub foreground: Option<[u8; 3]>,
    pub background: Option<[u8; 3]>,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
    #[serde(default)]
    pub underline: bool,
    #[serde(default)]
    pub inverse: bool,
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
    pub placement: UiRegionPlacement,
    pub lines: Vec<Vec<UiTextSpan>>,
    pub cursor: Option<[u16; 2]>,
    pub width: u16,
    pub row: Option<u16>,
    pub column: Option<u16>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiSurfaceFrame {
    pub owner: String,
    pub regions: Vec<UiRegion>,
    #[serde(default)]
    pub capture_input: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiSurfaceSnapshot {
    pub columns: u16,
    pub rows: u16,
    pub editor_text: String,
    pub tools_expanded: bool,
}
impl Default for UiSurfaceSnapshot {
    fn default() -> Self {
        Self {
            columns: 80,
            rows: 24,
            editor_text: String::new(),
            tools_expanded: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum UiSurfaceEvent {
    Refresh {
        snapshot: UiSurfaceSnapshot,
    },
    Input {
        data: String,
        sequence: u64,
        snapshot: UiSurfaceSnapshot,
    },
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum UiSurfaceAction {
    Wake,
    Close,
    EditorText { text: String, append: bool },
    ToolsExpanded { expanded: bool },
    Title { title: String },
    ForwardInput { sequence: u64 },
    Submit { text: String },
}
