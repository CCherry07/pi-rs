//! Session-local tool authorization. The hook waits without blocking ACP input;
//! the prompt task owns protocol dispatch and tool-update ordering.

use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::{Client, ConnectionTo, schema::v1 as acp};
use pi_plugin::{
    AgentPluginContext, Plugin, PluginError, PluginId, ToolCallBlock, ToolCallEvent, ToolCallPatch,
};
use pi_session::SessionGenerationOverlay;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use super::{now_ms, tool_kind};

#[cfg(test)]
mod tests;

const ALLOW_ONCE: &str = "allow-once";
const REJECT_ONCE: &str = "reject-once";
const TIMED_OUT: &str = "Tool permission request timed out; operation was not executed";
const CANCELLED: &str = "Tool permission request cancelled; operation was not executed";

pub(super) struct PermissionGate {
    timeout: Duration,
    requests: mpsc::UnboundedSender<PendingPermission>,
}

impl PermissionGate {
    pub(super) fn attach(
        overlay: SessionGenerationOverlay,
        timeout: Option<Duration>,
    ) -> (
        SessionGenerationOverlay,
        mpsc::UnboundedReceiver<PendingPermission>,
    ) {
        let (requests, receiver) = mpsc::unbounded_channel();
        let overlay = match timeout {
            Some(timeout) => overlay.with_plugin(move || {
                Arc::new(Self {
                    timeout,
                    requests: requests.clone(),
                })
            }),
            None => overlay,
        };
        (overlay, receiver)
    }
}

#[pi_plugin::plugin]
impl Plugin for PermissionGate {
    fn id(&self) -> PluginId {
        PluginId::new("acp-tool-permissions")
    }

    async fn tool_call(
        &self,
        context: AgentPluginContext,
        event: ToolCallEvent,
    ) -> Result<ToolCallPatch, PluginError> {
        let Some(deadline) = Instant::now().checked_add(self.timeout) else {
            return Ok(blocked("Invalid tool permission timeout"));
        };
        if context.signal().is_aborted() {
            return Ok(blocked(CANCELLED));
        }
        let (response, result) = oneshot::channel();
        let pending = PendingPermission {
            tool_call: acp::ToolCallUpdate::new(
                event.tool_call.id.as_str().to_string(),
                acp::ToolCallUpdateFields::new()
                    .title(event.tool_call.name.clone())
                    .kind(tool_kind(&event.tool_call.name))
                    .status(acp::ToolCallStatus::Pending)
                    .raw_input(event.validated_args),
            ),
            deadline,
            deadline_ms: now_ms()
                .saturating_add(i64::try_from(self.timeout.as_millis()).unwrap_or(i64::MAX)),
            response,
        };
        if self.requests.send(pending).is_err() {
            return Ok(blocked("ACP permission channel is unavailable"));
        }
        let decision = tokio::select! {
            biased;
            () = context.signal().wait() => Err(CANCELLED.to_string()),
            () = tokio::time::sleep_until(deadline) => Err(TIMED_OUT.to_string()),
            decision = result => decision.unwrap_or_else(|_| Err(CANCELLED.to_string())),
        };
        // A response queued before cancellation/expiry may be polled afterwards.
        // Never turn a late allow into permission to execute.
        Ok(if context.signal().is_aborted() {
            blocked(CANCELLED)
        } else if Instant::now() >= deadline {
            blocked(TIMED_OUT)
        } else {
            match decision {
                Ok(()) => ToolCallPatch::default(),
                Err(reason) => blocked(reason),
            }
        })
    }
}

fn blocked(reason: impl Into<String>) -> ToolCallPatch {
    ToolCallPatch {
        block: Some(ToolCallBlock {
            reason: reason.into(),
            terminate: false,
        }),
        ..ToolCallPatch::default()
    }
}

pub(super) struct PendingPermission {
    tool_call: acp::ToolCallUpdate,
    deadline: Instant,
    deadline_ms: i64,
    response: oneshot::Sender<Result<(), String>>,
}

impl PendingPermission {
    pub(super) fn dispatch(
        mut self,
        connection: &ConnectionTo<Client>,
        session_id: acp::SessionId,
    ) -> agent_client_protocol::Result<()> {
        if self.response.is_closed() || Instant::now() >= self.deadline {
            return Ok(());
        }
        let client = connection.clone();
        connection.spawn(async move {
            if self.response.is_closed() || Instant::now() >= self.deadline {
                return Ok(());
            }
            let request = acp::RequestPermissionRequest::new(
                session_id.clone(),
                self.tool_call.clone(),
                vec![
                    acp::PermissionOption::new(ALLOW_ONCE, "Allow once", acp::PermissionOptionKind::AllowOnce),
                    acp::PermissionOption::new(REJECT_ONCE, "Reject", acp::PermissionOptionKind::RejectOnce),
                ],
            ).meta(serde_json::Map::from_iter([
                ("pi-rs.dev/permission_deadline_ms".to_string(), self.deadline_ms.into()),
            ]));
            // This task is outside the SDK's ordered dispatcher: waiting for
            // the reverse request cannot block permission replies or cancel.
            let decision = tokio::select! {
                biased;
                () = self.response.closed() => return Ok(()),
                () = tokio::time::sleep_until(self.deadline) => Err(TIMED_OUT.to_string()),
                response = client.send_request(request).block_task() => match response {
                    Ok(response) => match response.outcome {
                        acp::RequestPermissionOutcome::Selected(selected) if selected.option_id.0.as_ref() == ALLOW_ONCE => Ok(()),
                        acp::RequestPermissionOutcome::Selected(selected) if selected.option_id.0.as_ref() == REJECT_ONCE => Err("Tool permission denied by client".to_string()),
                        acp::RequestPermissionOutcome::Cancelled => Err(CANCELLED.to_string()),
                        _ => Err("Client returned an unoffered permission option".to_string()),
                    },
                    Err(error) => Err(format!("Tool permission request failed: {error}")),
                },
            };
            if decision.is_ok() && !self.response.is_closed() && Instant::now() < self.deadline {
                self.tool_call.fields.status = Some(acp::ToolCallStatus::InProgress);
                client.send_notification(acp::SessionNotification::new(
                    session_id, acp::SessionUpdate::ToolCallUpdate(self.tool_call),
                ))?;
            }
            let _ = self.response.send(decision);
            Ok(())
        })
    }
}
