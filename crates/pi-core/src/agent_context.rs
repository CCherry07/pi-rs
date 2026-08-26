use crate::Message;

/// Immutable agent state exposed to lifecycle and tool hooks.
///
/// The runtime wraps this value in [`std::sync::Arc`] while a turn is in
/// flight, so every hook for the same tool batch observes one consistent
/// transcript without cloning it per plugin.
#[derive(Debug, Clone)]
pub struct AgentContext {
    pub system_prompt: String,
    pub messages: Vec<Message>,
    pub active_tools: Vec<String>,
}
