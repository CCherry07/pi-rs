//! Decision #205: genuine pre-initialize Ping replies only; all other RPC dispatch stays in rmcp.
use super::*;
use rmcp::model::{ClientResult, ServerNotification, ServerRequest};

/// rmcp's handshake dispatches logging but drops other notifications. Keep those genuine
/// messages until its normal dispatcher starts; never answer an RPC or fabricate a message.
/// The queue is response-local (not an ID map/recovery channel) and has both byte and item bounds.
#[derive(Default)]
pub(super) struct SetupNotifications {
    messages: Vec<ServerJsonRpcMessage>,
    bytes: usize,
}

impl SetupNotifications {
    pub(super) fn capture(
        &mut self,
        message: ServerJsonRpcMessage,
        byte_limit: usize,
    ) -> Result<Option<ServerJsonRpcMessage>, HttpError> {
        if !matches!(&message, ServerJsonRpcMessage::Notification(notification)
            if !matches!(notification.notification, ServerNotification::LoggingMessageNotification(_)))
        {
            return Ok(Some(message));
        }
        let bytes = serde_json::to_vec(&message)
            .map_err(|_| {
                HttpError::Client(PiHttpClientError::protocol(
                    "Invalid MCP setup notification",
                ))
            })?
            .len();
        if self.messages.len() >= 1024 || bytes > byte_limit.saturating_sub(self.bytes) {
            return Err(HttpError::Client(PiHttpClientError::protocol(
                "MCP setup notification prefix exceeded its bound",
            )));
        }
        self.bytes += bytes;
        self.messages.push(message);
        Ok(None)
    }

    pub(super) async fn forward<C: HttpClient>(
        &mut self,
        shared: &Shared<C>,
        session: &Session,
        id: &RequestId,
    ) -> Result<(), HttpError> {
        for message in self.messages.drain(..) {
            shared.forward(message, session, Some(id)).await?;
        }
        self.bytes = 0;
        Ok(())
    }
}

impl<C: HttpClient> Shared<C> {
    /// Every candidate has its own SDK service/transport. Setup has not admitted
    /// application requests yet; the surrounding setup task owns cancellation.
    pub(super) async fn bootstrap_ping(
        &self,
        message: &ServerJsonRpcMessage,
        session: &Session,
    ) -> Result<bool, HttpError> {
        let ServerJsonRpcMessage::Request(request) = message else {
            return Ok(false);
        };
        if !matches!(request.request, ServerRequest::PingRequest(_)) {
            return Ok(false);
        }
        let pong = ClientJsonRpcMessage::response(
            ClientResult::EmptyResult(().into()),
            request.id.clone(),
        );
        // This is an actual response to the received Ping, never a synthesized result
        // for an initialization or application request. Use the same auth backend and
        // the stream's session, not a stale control queued for another session.
        tokio::time::timeout(
            self.config.control_request_timeout,
            self.post_on_stream(pong, session),
        )
        .await
        .map_err(|_| HttpError::ControlRequestTimeout)??;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn progress() -> ServerJsonRpcMessage {
        serde_json::from_value(json!({"jsonrpc":"2.0","method":"notifications/progress",
            "params":{"progressToken":"private-prefix-marker","progress":1}}))
        .unwrap()
    }

    fn assert_limit(error: HttpError) {
        assert!(error.to_string().contains("prefix exceeded its bound"));
        assert!(!format!("{error:?}").contains("private-prefix-marker"));
        let HttpError::Client(error) = error else {
            panic!("typed client failure expected")
        };
        assert!(error.status().is_none());
        assert!(error.network_flags().is_none());
    }

    #[test]
    fn setup_notification_buffer_is_item_and_byte_bounded_without_retry_authority() {
        let mut queue = SetupNotifications::default();
        for _ in 0..1024 {
            assert!(queue.capture(progress(), usize::MAX).unwrap().is_none());
        }
        assert_limit(queue.capture(progress(), usize::MAX).unwrap_err());
        assert_eq!(queue.messages.len(), 1024);
        let bytes = serde_json::to_vec(&progress()).unwrap().len();
        let mut queue = SetupNotifications::default();
        assert!(queue.capture(progress(), bytes).unwrap().is_none());
        assert_limit(queue.capture(progress(), bytes).unwrap_err());
        assert_eq!(queue.messages.len(), 1);
        assert_eq!(queue.bytes, bytes);
    }

    #[test]
    fn setup_logging_and_rpc_remain_in_the_sdk_handshake_path() {
        let mut queue = SetupNotifications::default();
        for message in [
            json!({"jsonrpc":"2.0","method":"notifications/message","params":{"level":"info","data":"setup log"}}),
            json!({"jsonrpc":"2.0","method":"roots/list","id":"actual-root-request"}),
            json!({"jsonrpc":"2.0","id":"actual-error","error":{"code":-32601,"message":"actual error"}}),
        ] {
            let wire = serde_json::from_value(message.clone()).unwrap();
            let forwarded = queue.capture(wire, 0).unwrap().unwrap();
            assert_eq!(serde_json::to_value(forwarded).unwrap(), message);
        }
        assert!(queue.messages.is_empty());
        assert_eq!(queue.bytes, 0);
    }
}
