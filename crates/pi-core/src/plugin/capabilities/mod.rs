//! Generation-bound capabilities exposed to plugin callbacks.
//!
//! `pi-core` owns this contract because agent, tool, command, provider, and
//! session callback traits all depend on it. The production behavior is
//! implemented by `pi-session`; JavaScript wire operations live outside this
//! module in `pi-js-plugin`.

mod epoch;
mod models;
mod session;
mod types;
mod ui;

use async_trait::async_trait;

pub use epoch::{PluginContextEpoch, PluginContextHandle, PluginContextReplacement};
pub use models::{CommandModelsContext, ModelsContext, ModelsContextAccess};
pub use session::{
    CommandSessionContext, DirectCompletionRequest, EphemeralCompactionOptions,
    EphemeralSessionOutcome, EphemeralSessionRequest, EphemeralSessionStatus,
    IsolatedSessionHandle, IsolatedSessionId, IsolatedSessionOptions, IsolatedSessionOutcome,
    IsolatedSessionRequest, ReplacedSessionContext, SessionContext, SessionContextAccess,
    SessionEntryKind, SessionEntryView, SessionExecutionOrigin, SessionReplacement,
    SessionSnapshot,
};
pub use types::{
    CompactOptions, ContextUsage, ForkOptions, ForkPosition, MessageDelivery, NavigateTreeOptions,
    NewSessionOptions, NoticeLevel, PluginContextError, PluginContextResult, PluginContextScope,
    PresentationMode, ScopedModel, SendMessageOptions, SendUserMessageOptions,
};
pub use ui::{
    UiContext, UiContextAccess, UiMultiSelectAction, UiMultiSelectOption, UiMultiSelectRequest,
    UiMultiSelectResponse,
};

/// Dependency-inward aggregate implemented by the Pi-owned context Adapter.
///
/// The actual interface is split by domain into `SessionContextAccess`,
/// `ModelsContextAccess`, and `UiContextAccess`. Native plugins receive only
/// the corresponding typed capability objects.
#[doc(hidden)]
pub trait PluginContext:
    SessionContextAccess + ModelsContextAccess + UiContextAccess + Send + Sync
{
}

impl<T> PluginContext for T where
    T: SessionContextAccess + ModelsContextAccess + UiContextAccess + Send + Sync
{
}

/// Domain capabilities shared by ordinary native callback contexts.
///
/// Callers receive the fields directly on their callback context; this helper
/// only carries them between the runtime generation and the owning crate.
#[doc(hidden)]
#[derive(Clone)]
pub struct ContextParts {
    pub session: SessionContext,
    pub models: ModelsContext,
    pub ui: UiContext,
}

impl ContextParts {
    pub fn new(handle: PluginContextHandle) -> Self {
        Self {
            session: SessionContext::from_handle(handle.clone()),
            models: ModelsContext::from_handle(handle.clone()),
            ui: UiContext::new(handle),
        }
    }

    pub fn unavailable() -> Self {
        PluginContextEpoch::unavailable().context()
    }
}

/// Stronger domain capabilities available to registered command callbacks.
#[doc(hidden)]
#[derive(Clone)]
pub struct CommandContextParts {
    pub session: CommandSessionContext,
    pub models: CommandModelsContext,
    pub ui: UiContext,
}

impl CommandContextParts {
    pub fn new(handle: PluginContextHandle) -> Self {
        Self {
            session: CommandSessionContext::from_handle(handle.clone()),
            models: CommandModelsContext::from_handle(handle.clone()),
            ui: UiContext::new(handle),
        }
    }

    pub fn unavailable() -> Self {
        PluginContextEpoch::unavailable().command_context()
    }
}

#[doc(hidden)]
pub struct UnavailablePluginContext;

#[async_trait]
impl SessionContextAccess for UnavailablePluginContext {}

#[async_trait]
impl ModelsContextAccess for UnavailablePluginContext {}

#[async_trait]
impl UiContextAccess for UnavailablePluginContext {}

#[cfg(test)]
mod tests;
