//! Generation lifetime and scope enforcement for plugin capabilities.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{
    CommandContextParts, ContextParts, PluginContext, PluginContextError, PluginContextResult,
    PluginContextScope, UnavailablePluginContext,
};

struct PluginContextEpochInner {
    active: AtomicBool,
    access: Arc<dyn PluginContext>,
}

/// Generation-owned lifetime guard shared by native and JavaScript contexts.
#[derive(Clone)]
pub struct PluginContextEpoch {
    inner: Arc<PluginContextEpochInner>,
}

impl PluginContextEpoch {
    pub fn new(access: Arc<dyn PluginContext>) -> Self {
        Self {
            inner: Arc::new(PluginContextEpochInner {
                active: AtomicBool::new(true),
                access,
            }),
        }
    }

    pub fn unavailable() -> Self {
        Self::new(Arc::new(UnavailablePluginContext))
    }

    pub fn handle(&self, scope: PluginContextScope) -> PluginContextHandle {
        PluginContextHandle {
            epoch: self.clone(),
            scope,
        }
    }

    pub fn context(&self) -> ContextParts {
        ContextParts::new(self.handle(PluginContextScope::Base))
    }

    pub fn command_context(&self) -> CommandContextParts {
        CommandContextParts::new(self.handle(PluginContextScope::Command))
    }

    pub fn retire(&self) {
        self.inner.active.store(false, Ordering::Release);
    }

    fn ensure_active(&self) -> PluginContextResult<()> {
        self.inner
            .active
            .load(Ordering::Acquire)
            .then_some(())
            .ok_or(PluginContextError::Retired)
    }
}

/// Cloneable generation-bound handle used by outer adapters and typed Rust capabilities.
#[derive(Clone)]
pub struct PluginContextHandle {
    epoch: PluginContextEpoch,
    scope: PluginContextScope,
}

impl PluginContextHandle {
    pub(super) fn access(&self) -> PluginContextResult<Arc<dyn PluginContext>> {
        self.epoch.ensure_active()?;
        Ok(Arc::clone(&self.epoch.inner.access))
    }

    /// Raw access for outer protocol adapters. Native plugin authors receive
    /// the typed `session`, `models`, and `ui` capabilities instead.
    #[doc(hidden)]
    pub fn access_for_adapter(&self) -> PluginContextResult<Arc<dyn PluginContext>> {
        self.access()
    }

    pub fn scope(&self) -> PluginContextScope {
        self.scope
    }
}

/// Explicit capability handoff after a session-replacing command. The old
/// generation may retire while the command is awaiting, so callers must use
/// this freshly resolved handle for replacement-session work.
#[doc(hidden)]
#[derive(Clone)]
pub struct PluginContextReplacement {
    pub cancelled: bool,
    pub context: Option<PluginContextHandle>,
}
