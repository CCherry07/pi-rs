use super::*;

/// Messages reduced into application state by the TUI model.
///
/// The event loop translates transport-specific notifications into this enum;
/// rendering modules only observe the resulting `App` state.
pub(super) enum AppMessage {
    SessionEvent {
        event: Box<AgentSessionEvent>,
        snapshot: Box<AgentSessionSnapshot>,
    },
    EffectCompleted(EffectDone),
    TrustRequested(ProjectTrustPromptRequest),
    ConfirmationRequested(PluginConfirmationRequest),
    SelectionRequested(PluginSelectionRequest),
    MultiSelectionRequested(PluginMultiSelectionRequest),
    AnimationTick,
    Quit,
}
