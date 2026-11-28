//! Per-fixture SDK callback observation. Absent from production builds; never recovery state.
use super::*;
use futures::future::BoxFuture;
use std::sync::Arc;

type Ping =
    dyn Fn(RequestContext<RoleClient>) -> BoxFuture<'static, Result<(), ErrorData>> + Send + Sync;
struct Hooks {
    ping: Box<Ping>,
    progress: Box<dyn Fn(f64) + Send + Sync>,
}

#[derive(Clone)]
pub(crate) struct Callbacks {
    hooks: Arc<Hooks>,
    pub(crate) control_timeout: Option<std::time::Duration>,
}
impl Callbacks {
    pub(crate) fn new(
        ping: impl Fn(RequestContext<RoleClient>) -> BoxFuture<'static, Result<(), ErrorData>>
        + Send
        + Sync
        + 'static,
        progress: impl Fn(f64) + Send + Sync + 'static,
    ) -> Self {
        Self {
            hooks: Arc::new(Hooks {
                ping: Box::new(ping),
                progress: Box::new(progress),
            }),
            control_timeout: None,
        }
    }
    pub(crate) fn with_control_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.control_timeout = Some(timeout);
        self
    }

    pub(super) fn ping(
        &self,
        context: RequestContext<RoleClient>,
    ) -> BoxFuture<'static, Result<(), ErrorData>> {
        (self.hooks.ping)(context)
    }
    pub(super) fn progress(&self, value: f64) {
        (self.hooks.progress)(value);
    }
}
impl PartialEq for Callbacks {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.hooks, &other.hooks) && self.control_timeout == other.control_timeout
    }
}
impl Eq for Callbacks {}
impl std::fmt::Debug for Callbacks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TestCallbacks")
    }
}
