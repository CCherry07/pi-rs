//! Session roots and non-blocking progress delivery for MCP calls.
// Roots remain part of the stable Pi MCP contract, although the newer SDK deprecates them.
#![allow(deprecated)]

use std::path::Path;

use rmcp::model::{ClientInfo, ListRootsResult, ProgressNotificationParam, Root};
use rmcp::service::{NotificationContext, RequestContext};
use rmcp::{ClientHandler, ErrorData, RoleClient};
use tokio::sync::broadcast;

#[derive(Clone)]
pub(crate) struct McpClientHandler {
    roots: Vec<Root>,
    pub(crate) progress: broadcast::Sender<ProgressNotificationParam>,
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
            progress: broadcast::channel(64).0,
        }
    }
}

impl ClientHandler for McpClientHandler {
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

    async fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        // A stalled frontend must never block protocol response processing.
        let _ = self.progress.send(params);
    }
}
