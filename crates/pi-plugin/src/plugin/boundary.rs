//! Actionable session boundaries. Storage validation and projection are supplied by the owner.
use super::{AgentHook, PluginDriver, TurnEndEvent};
use crate::{AbortSignal, AgentPluginContext, Message, RunId};
use pi_core::session::{ContextEditEntry, CustomEntry, CustomMessageEntry};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum SessionBoundaryDraft {
    Custom(CustomEntry),
    CustomMessage(CustomMessageEntry),
    ContextEdit(ContextEditEntry),
    Compaction(BoundaryCompaction),
    /// Preserves a malformed adapter proposal for a later handler to repair.
    Invalid(Value),
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ValidBoundaryDraft {
    Custom(CustomEntry),
    CustomMessage(CustomMessageEntry),
    ContextEdit(ContextEditEntry),
    Compaction(BoundaryCompaction),
}
impl Serialize for SessionBoundaryDraft {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let value = match self {
            Self::Custom(v) => ValidBoundaryDraft::Custom(v.clone()),
            Self::CustomMessage(v) => ValidBoundaryDraft::CustomMessage(v.clone()),
            Self::ContextEdit(v) => ValidBoundaryDraft::ContextEdit(v.clone()),
            Self::Compaction(v) => ValidBoundaryDraft::Compaction(v.clone()),
            Self::Invalid(v) => return v.serialize(serializer),
        };
        value.serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for SessionBoundaryDraft {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Ok(
            match serde_json::from_value::<ValidBoundaryDraft>(value.clone()) {
                Ok(ValidBoundaryDraft::Custom(v)) => Self::Custom(v),
                Ok(ValidBoundaryDraft::CustomMessage(v)) => Self::CustomMessage(v),
                Ok(ValidBoundaryDraft::ContextEdit(v)) => Self::ContextEdit(v),
                Ok(ValidBoundaryDraft::Compaction(v)) => Self::Compaction(v),
                Err(_) => Self::Invalid(value),
            },
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundaryCompaction {
    pub summary: String,
    #[serde(deserialize_with = "required_kept_id")]
    pub first_kept_entry_id: Option<String>,
    pub details: Option<Value>,
    pub usage: Option<crate::Usage>,
}

fn required_kept_id<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentActivityOutcome {
    Completed,
    Error,
    Aborted,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundaryContextPreview {
    /// Provenance-preserving entries before provider conversion.
    pub context_entries: Vec<pi_core::session::ProjectedSessionEntry>,
    pub context_messages: Vec<pi_core::session::AgentMessage>,
    pub llm_messages: Vec<Message>,
    pub pending_messages: Vec<Message>,
    pub can_continue: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundaryState {
    pub entries: Vec<SessionBoundaryDraft>,
    #[serde(rename = "continue")]
    pub continue_run: bool,
    pub context: BoundaryContextPreview,
    pub outcome: AgentActivityOutcome,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BoundaryResult {
    pub entries: Option<Vec<SessionBoundaryDraft>>,
    #[serde(rename = "continue")]
    pub continue_run: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentBeforeSettleEvent {
    pub boundary: BoundaryState,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SessionBoundaryEvent {
    TurnEnd(TurnEndEvent),
    BeforeSettle(AgentBeforeSettleEvent),
}

impl SessionBoundaryEvent {
    pub fn state(&self) -> &BoundaryState {
        match self {
            Self::TurnEnd(event) => event.boundary.as_ref().expect("session boundary state"),
            Self::BeforeSettle(event) => &event.boundary,
        }
    }
}

impl PluginDriver {
    pub fn has_boundary_hook(&self, hook: AgentHook) -> bool {
        self.interested_plugins(hook).next().is_some()
    }
    /// Rebuild the preview after each handler, including failed handlers. Invalid proposals
    /// remain visible to later handlers for repair; a final invalid batch is discarded.
    pub async fn dispatch_boundary(
        &self,
        run_id: &RunId,
        cwd: &std::path::Path,
        signal: &AbortSignal,
        turn: Option<TurnEndEvent>,
        outcome: AgentActivityOutcome,
        preview: impl Fn(&[SessionBoundaryDraft]) -> Result<BoundaryContextPreview, String>
        + Send
        + Sync,
    ) -> BoundaryResult {
        let hook = if turn.is_some() {
            AgentHook::TurnEnd
        } else {
            AgentHook::AgentBeforeSettle
        };
        let Ok(context) = preview(&[]) else {
            return BoundaryResult::default();
        };
        let mut state = BoundaryState {
            entries: Vec::new(),
            continue_run: false,
            context,
            outcome,
        };
        let mut valid = true;
        for registered in self.interested_plugins(hook) {
            for index in 0..registered.plugin.boundary_hook_count(hook) {
                let event = match &turn {
                    Some(turn) => {
                        let mut turn = turn.clone();
                        turn.boundary = Some(state.clone());
                        SessionBoundaryEvent::TurnEnd(turn)
                    }
                    None => SessionBoundaryEvent::BeforeSettle(AgentBeforeSettleEvent {
                        boundary: state.clone(),
                    }),
                };
                let context: AgentPluginContext =
                    self.agent_plugin_context(registered, run_id, cwd, signal);
                match registered.plugin.boundary_hook(context, event, index).await {
                    Ok(result) => {
                        if let Some(entries) = result.entries {
                            state.entries = entries;
                        }
                        if let Some(value) = result.continue_run {
                            state.continue_run = value;
                        }
                    }
                    Err(error) => self.diagnostics.record(
                        registered.id.clone(),
                        hook,
                        None,
                        error.to_string(),
                    ),
                }
                match preview(&state.entries) {
                    Ok(context) => {
                        state.context = context;
                        valid = true;
                    }
                    Err(error) => {
                        valid = false;
                        self.diagnostics.record(
                            registered.id.clone(),
                            hook,
                            None,
                            format!("Invalid boundary entries: {error}"),
                        );
                    }
                }
            }
        }
        if valid {
            BoundaryResult {
                entries: Some(state.entries),
                continue_run: Some(state.continue_run),
            }
        } else {
            BoundaryResult::default()
        }
    }

    pub fn report_boundary_error(&self, hook: AgentHook, message: impl Into<String>) {
        self.diagnostics.record(
            crate::PluginId::new("session-boundary"),
            hook,
            None,
            message,
        );
    }
}
