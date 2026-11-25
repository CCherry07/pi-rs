//! SDK-backed notification subscription and cancellable refresh-only discovery.
//! The SDK's list_all_tools drops only the response receiver on timeout, not the pending
//! request. Refresh therefore uses its typed cancellable handles and one deadline across pages.
use super::*;
use rmcp::model::{
    ListToolsRequest, PaginatedRequestParams, ProtocolVersion, ServerNotification,
    SubscriptionFilter,
};
use rmcp::service::RequestHandle;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::task::JoinHandle;

#[derive(Default)]
pub(super) struct DiscoveryControl {
    task: std::sync::Mutex<Option<JoinHandle<()>>>,
    error: Arc<std::sync::Mutex<Option<&'static str>>>,
    cancelling: Arc<AtomicBool>,
}

impl DiscoveryControl {
    pub(super) fn cancel(&self) {
        if let Some(task) = self
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            // Dropping the SDK Subscription sends its native cancellation notification.
            task.abort();
        }
    }
}

impl Drop for DiscoveryControl {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl McpConnection {
    pub(super) async fn subscribe_tools(&self) {
        if !self.metadata().is_some_and(|info| {
            info.protocol_version.as_str() >= ProtocolVersion::V_2026_07_28.as_str()
                && info
                    .capabilities
                    .tools
                    .as_ref()
                    .and_then(|tools| tools.list_changed)
                    == Some(true)
        }) {
            return;
        }
        // This runs inside the existing total connection deadline, before initial tools/list.
        // PendingSubscriptionRequest also sends cancellation if that deadline drops listen.
        let subscription = self
            .peer
            .listen(SubscriptionFilter::builder().tools_list_changed().build())
            .await;
        let notifications = self.handler.tools_changed.clone();
        let error = self.discovery.error.clone();
        let mut subscription = match subscription {
            Ok(subscription) if subscription.acknowledged().tools_list_changed == Some(true) => {
                subscription
            }
            _ => {
                *error
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(
                    "tool change subscription unavailable; last discovered tools retained; reconnect to retry",
                );
                notifications.send_modify(|revision| *revision = revision.wrapping_add(1));
                return;
            }
        };
        // No connection/source/supervisor Arc is captured. Raw SDK subscription owns only peer
        // transport state, and is aborted by raw connection cancellation, even with old views.
        let task = tokio::spawn(async move {
            while let Ok(Some(notification)) = subscription.next().await {
                if matches!(
                    notification,
                    ServerNotification::ToolListChangedNotification(_)
                ) {
                    notifications.send_modify(|revision| *revision = revision.wrapping_add(1));
                }
            }
            *error
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(
                "tool change subscription ended; last discovered tools retained; reconnect to retry",
            );
            notifications.send_modify(|revision| *revision = revision.wrapping_add(1));
        });
        *self
            .discovery
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(task);
    }

    fn refresh_error(&self, message: &'static str) -> McpError {
        McpError::ListTools {
            server: self.name.clone(),
            message: message.into(),
        }
    }

    pub(crate) async fn refresh_tools(
        &self,
        budget: Duration,
    ) -> Result<Vec<McpToolSpec>, McpError> {
        if let Some(message) = *self
            .discovery
            .error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            return Err(self.refresh_error(message));
        }
        if self.discovery.cancelling.load(Ordering::Acquire) {
            return Err(self.refresh_error("tool refresh cancellation incomplete; last discovered tools retained; reconnect before refreshing"));
        }
        let deadline = tokio::time::sleep(budget);
        tokio::pin!(deadline);
        let timeout = || {
            self.refresh_error("tool inventory refresh timed out; last discovered tools retained")
        };
        let failed =
            || self.refresh_error("tool inventory refresh failed; last discovered tools retained");
        let mut tools = Vec::new();
        let mut cursor = None;
        let mut seen = HashSet::new();
        for _ in 0..1000 {
            let request = ClientRequest::ListToolsRequest(ListToolsRequest::with_param(
                PaginatedRequestParams::default().with_cursor(cursor.take()),
            ));
            let handle = tokio::select! {
                biased;
                () = &mut deadline => return Err(timeout()),
                result = self.peer.send_cancellable_request(request, rmcp::service::PeerRequestOptions::no_options()) => result.map_err(|_| failed())?,
            };
            let mut request = CancelOnDrop {
                handle: Some(handle),
                cancelling: self.discovery.cancelling.clone(),
            };
            let response = tokio::select! {
                biased;
                () = &mut deadline => {
                    if !request.cancel().await {
                        return Err(self.refresh_error("tool refresh cancellation incomplete; last discovered tools retained; reconnect before refreshing"));
                    }
                    return Err(timeout());
                }
                response = &mut request.handle.as_mut().expect("active request").rx => response,
            };
            request.handle.take(); // The response finished this request; disarm cancellation.
            let result = response.map_err(|_| failed())?.map_err(|_| failed())?;
            let ServerResult::ListToolsResult(page) = result else {
                return Err(failed());
            };
            tools.extend(page.tools);
            let Some(next) = page.next_cursor else {
                return Ok(tools);
            };
            if !seen.insert(next.clone()) {
                return Err(self.refresh_error(
                    "tool inventory repeated a pagination cursor; last discovered tools retained",
                ));
            }
            cursor = Some(next);
        }
        Err(self.refresh_error(
            "tool inventory exceeded pagination limit; last discovered tools retained",
        ))
    }
}

struct CancelOnDrop {
    handle: Option<RequestHandle<RoleClient>>,
    cancelling: Arc<AtomicBool>,
}

impl CancelOnDrop {
    fn cleanup(&mut self) -> Option<JoinHandle<bool>> {
        let handle = self.handle.take()?;
        self.cancelling.store(true, Ordering::Release);
        let cancelling = self.cancelling.clone();
        // Bounded control work owns only the SDK handle/ID and a transport-only flag. If its
        // caller is aborted, cancellation still runs; failure blocks overlapping future lists.
        Some(tokio::spawn(async move {
            let completed = matches!(
                tokio::time::timeout(
                    SHUTDOWN_TIMEOUT,
                    handle.cancel(Some("tool inventory refresh cancelled".into()))
                )
                .await,
                Ok(Ok(()))
            );
            if completed {
                cancelling.store(false, Ordering::Release);
            }
            completed
        }))
    }

    async fn cancel(mut self) -> bool {
        match self.cleanup() {
            Some(task) => task.await.unwrap_or(false),
            None => true,
        }
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.handle.is_some() && tokio::runtime::Handle::try_current().is_ok() {
            self.cleanup();
        }
    }
}
