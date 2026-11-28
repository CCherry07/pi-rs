//! Session roots and non-blocking progress delivery for MCP calls.
// Roots remain part of the stable Pi MCP contract, although the newer SDK deprecates them.
#![allow(deprecated)]

use std::path::Path;

use crate::diagnostics::{ServerLog, StderrCapture};
use rmcp::model::{
    ClientInfo, ListRootsResult, LoggingMessageNotificationParam, ProgressNotificationParam, Root,
};
use rmcp::service::{NotificationContext, RequestContext};
use rmcp::{ClientHandler, ErrorData, RoleClient};
use tokio::sync::{broadcast, watch};

#[cfg(test)]
pub(crate) mod testing;

#[derive(Clone)]
pub(crate) struct McpClientHandler {
    roots: Vec<Root>,
    pub(crate) log: Option<ServerLog>,
    pub(crate) stderr: StderrCapture,
    pub(crate) progress: broadcast::Sender<ProgressNotificationParam>,
    pub(crate) tools_changed: watch::Sender<u64>,
    #[cfg(test)]
    pub(crate) callbacks: Option<testing::Callbacks>,
}

impl McpClientHandler {
    pub(crate) fn new(cwd: Option<&Path>) -> Self {
        let fallback = std::env::current_dir().ok();
        let roots = cwd
            .or(fallback.as_deref())
            .and_then(|path| {
                let uri = url::Url::from_file_path(path).ok()?;
                let root = Root::new(uri.to_string());
                Some(match path.file_name() {
                    Some(name) => root.with_name(name.to_string_lossy()),
                    None => root,
                })
            })
            .into_iter()
            .collect();
        Self {
            roots,
            log: None,
            stderr: StderrCapture::default(),
            progress: broadcast::channel(64).0,
            tools_changed: watch::channel(0).0,
            #[cfg(test)]
            callbacks: None,
        }
    }
}

impl ClientHandler for McpClientHandler {
    #[cfg(test)]
    async fn ping(&self, context: RequestContext<RoleClient>) -> Result<(), ErrorData> {
        match &self.callbacks {
            Some(callbacks) => callbacks.ping(context).await,
            None => ().ping(context).await,
        }
    }

    fn get_info(&self) -> ClientInfo {
        let mut info = ClientInfo::default();
        info.capabilities.roots = Some(Default::default());
        info.client_info.name = "pi".into();
        info.client_info.version = env!("CARGO_PKG_VERSION").into();
        info
    }

    async fn list_roots(
        &self,
        _context: RequestContext<RoleClient>,
    ) -> Result<ListRootsResult, ErrorData> {
        Ok(ListRootsResult::new(self.roots.clone()))
    }

    async fn on_logging_message(
        &self,
        params: LoggingMessageNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        if let Some(log) = &self.log {
            log.write(params);
        }
    }

    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        // Keep the revision even before a supervisor subscribes, including initial discovery.
        self.tools_changed
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }

    async fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        #[cfg(test)]
        if let Some(callbacks) = &self.callbacks {
            callbacks.progress(params.progress);
        }
        // A stalled frontend must never block protocol response processing.
        let _ = self.progress.send(params);
    }
}
