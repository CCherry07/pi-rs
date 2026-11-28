//! Ordered delivery of actual JSON members, with SDK-owned post-response dispatch.
use super::*;
use crate::http_client::{JsonMessageIter, JsonMessages};

impl<C: HttpClient> Shared<C> {
    pub(super) async fn json_response(
        &self,
        messages: JsonMessages,
        session: &Session,
        id: Option<&RequestId>,
        setup: bool,
        responder: &mut SendAck,
    ) -> Result<Tail, HttpError> {
        let Some(id) = id else {
            return Ok(None);
        };
        if setup
            && messages.is_batch()
            && let Some(responder) = responder.take()
        {
            // A complete real JSON document is available. Setup must receive before a
            // batch can fill the bounded channel; malformed members still stop the batch.
            let _ = responder.send(Ok(()));
        }
        let mut messages = messages.into_iter();
        let mut pending = bootstrap::SetupNotifications::default();
        while let Some(message) = messages.next() {
            let message = message?;
            if setup && self.bootstrap_ping(&message, session).await? {
                continue;
            }
            let message = if setup {
                let Some(message) = pending.capture(message, self.config.max_sse_event_size)?
                else {
                    continue;
                };
                // An actual error still belongs to the SDK handshake, including its
                // strict ID handling. Do not turn a pending prefix into a fake success.
                if matches!(message, ServerJsonRpcMessage::Error(_)) {
                    pending.forward(self, session, id).await?;
                }
                message
            } else {
                message
            };
            let done = answered(&message, id);
            self.forward(message, session, Some(id)).await?;
            if done {
                pending.forward(self, session, id).await?;
                // A later invalid member cannot fail an already delivered response.
                // The worker acknowledges the original send and retains cancellation
                // registration through the remaining genuine JSON members.
                return Ok(Some(ResponseTail::Json(
                    messages,
                    session.clone(),
                    id.clone(),
                )));
            }
        }
        // JSON send ends with the document, not necessarily a correlated reply.
        // In particular, keep a genuine setup error available to rmcp's typed
        // handshake/correlation handling rather than replacing it with send failure.
        // An empty/unrelated batch leaves the caller's existing deadline in charge.
        Ok(None)
    }

    pub(super) async fn drain_json_tail(
        &self,
        messages: JsonMessageIter,
        session: &Session,
        id: &RequestId,
    ) -> Result<(), HttpError> {
        for message in messages {
            self.forward(message?, session, Some(id)).await?;
        }
        Ok(())
    }
}
