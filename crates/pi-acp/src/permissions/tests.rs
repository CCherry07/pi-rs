use std::sync::Mutex as StdMutex;

use agent_client_protocol::{ByteStreams, Responder, on_receive_notification, on_receive_request};
use pi_agent::AgentOptions;
use pi_core::{ModelId, ProviderId, ToolCall};
use pi_runtime::PiRuntime;
use pi_session::{
    AgentSessionOptions, MultiSessionManager, PreparedSessionGeneration, SessionGenerationRequest,
};
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn, TestToolsPlugin};
use serde_json::json;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use super::*;
use crate::{AcpOptions, AcpServer, internal_error};

struct PatchArguments;

#[pi_plugin::plugin]
impl Plugin for PatchArguments {
    fn id(&self) -> PluginId {
        PluginId::new("patch-arguments")
    }

    async fn tool_call(
        &self,
        _context: AgentPluginContext,
        mut event: ToolCallEvent,
    ) -> Result<ToolCallPatch, PluginError> {
        event.validated_args["value"] = json!("approved-input");
        Ok(ToolCallPatch {
            arguments: Some(event.validated_args),
            ..ToolCallPatch::default()
        })
    }
}

fn manager(tools: TestToolsPlugin) -> MultiSessionManager {
    MultiSessionManager::new(move |request: SessionGenerationRequest| {
        let tools = tools.clone();
        async move {
            let builder = PiRuntime::builder()
                .provider_plugin(ScriptedProviderPlugin::scripted([
                    ScriptedTurn::ToolCalls(vec![ToolCall::new(
                        "call-1",
                        "delay",
                        json!({"value":"original"}),
                    )]),
                    ScriptedTurn::Text("done".into()),
                ]))
                .plugin(tools)
                .plugin(PatchArguments)
                .agent_options(AgentOptions {
                    provider_id: ProviderId::new("scripted"),
                    model_id: ModelId::new("test"),
                    cwd: request.cwd,
                    active_tools: vec!["delay".into()],
                    ..AgentOptions::default()
                });
            Ok(PreparedSessionGeneration::new(
                request.generation_overlay.apply_to(builder).build()?,
                AgentSessionOptions::default(),
            ))
        }
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Decision {
    Allow,
    Reject,
    Invalid,
    Cancelled,
    CancelTurn,
    Close,
    Timeout,
    Error,
    Disconnect,
    Disabled,
}

async fn exercise(decision: Decision) {
    let directory = tempfile::tempdir().unwrap();
    let tools = TestToolsPlugin::new();
    let manager = manager(tools.clone());
    let options = AcpOptions::new(directory.path());
    let options = if decision == Decision::Disabled {
        options
    } else {
        options.request_tool_permissions(Duration::from_millis(if decision == Decision::Timeout {
            300
        } else {
            5000
        }))
    };
    let server = AcpServer::new(manager.clone(), options);
    let updates = Arc::new(StdMutex::new(Vec::<acp::SessionNotification>::new()));
    let received = Arc::clone(&updates);
    let (requests, mut pending) = mpsc::unbounded_channel();
    let (client_writer, server_reader) = tokio::io::duplex(64 * 1024);
    let (server_writer, client_reader) = tokio::io::duplex(64 * 1024);
    let server_task = tokio::spawn(server.serve(ByteStreams::new(
        server_writer.compat_write(),
        server_reader.compat(),
    )));
    let client_transport = ByteStreams::new(client_writer.compat_write(), client_reader.compat());
    tokio::time::timeout(Duration::from_secs(10), Client.builder()
        .on_receive_request(async move |request: acp::RequestPermissionRequest, responder: Responder<acp::RequestPermissionResponse>, _connection| {
            requests.send((request, responder)).map_err(internal_error)
        }, on_receive_request!())
        .on_receive_notification(async move |notification: acp::SessionNotification, _connection| {
            received.lock().unwrap().push(notification);
            Ok(())
        }, on_receive_notification!())
        .connect_with(client_transport, async |connection| {
            connection.send_request(acp::InitializeRequest::new(agent_client_protocol::schema::ProtocolVersion::V1)).block_task().await?;
            let created = connection.send_request(acp::NewSessionRequest::new(directory.path().canonicalize().unwrap())).block_task().await?;
            let prompt = connection.send_request(acp::PromptRequest::new(created.session_id.clone(), vec![acp::ContentBlock::Text(acp::TextContent::new("act"))])).block_task();
            if decision == Decision::Disabled {
                assert_eq!(prompt.await?.stop_reason, acp::StopReason::EndTurn);
                assert!(pending.try_recv().is_err());
            } else {
                let (request, responder) = pending.recv().await.unwrap();
                assert_eq!(request.session_id, created.session_id);
                assert_eq!(request.tool_call.tool_call_id.0.as_ref(), "call-1");
                assert_eq!(request.tool_call.fields.raw_input, Some(json!({"value":"approved-input"})));
                assert_eq!(request.tool_call.fields.status, Some(acp::ToolCallStatus::Pending));
                assert_eq!(request.options.iter().map(|option| option.kind).collect::<Vec<_>>(), vec![acp::PermissionOptionKind::AllowOnce, acp::PermissionOptionKind::RejectOnce]);
                assert!(request.meta.unwrap()["pi-rs.dev/permission_deadline_ms"].as_i64().unwrap() > 0);
                assert!(tools.completions().is_empty(), "tool ran before approval");
                assert!(updates.lock().unwrap().iter().any(|notification| matches!(&notification.update, acp::SessionUpdate::ToolCall(call) if call.status == acp::ToolCallStatus::Pending)));
                let selected = |id| acp::RequestPermissionResponse::new(acp::RequestPermissionOutcome::Selected(acp::SelectedPermissionOutcome::new(id)));
                let mut late = None;
                match decision {
                    Decision::Allow => responder.respond(selected(ALLOW_ONCE))?,
                    Decision::Reject => responder.respond(selected(REJECT_ONCE))?,
                    Decision::Invalid => responder.respond(selected("allow-always"))?,
                    Decision::Cancelled => responder.respond(acp::RequestPermissionResponse::new(acp::RequestPermissionOutcome::Cancelled))?,
                    Decision::Error => responder.respond_with_result(Err(agent_client_protocol::Error::method_not_found()))?,
                    Decision::CancelTurn => {
                        connection.send_notification(acp::CancelNotification::new(created.session_id.clone()))?;
                        late = Some(responder);
                    }
                    Decision::Close => {
                        connection.send_request(acp::CloseSessionRequest::new(created.session_id.clone())).block_task().await?;
                        late = Some(responder);
                    }
                    Decision::Timeout => late = Some(responder),
                    Decision::Disconnect => { drop(responder); return Ok(()); }
                    Decision::Disabled => unreachable!(),
                }
                let response = prompt.await?;
                assert_eq!(response.stop_reason, if matches!(decision, Decision::CancelTurn | Decision::Close) { acp::StopReason::Cancelled } else { acp::StopReason::EndTurn });
                if let Some(responder) = late {
                    // Intentionally answer after the turn has settled. The old
                    // response must neither execute the tool nor break transport.
                    let _ = responder.respond(selected(ALLOW_ONCE));
                    connection.send_request(acp::ListSessionsRequest::new()).block_task().await?;
                }
            }
            if decision != Decision::Close {
                connection.send_request(acp::CloseSessionRequest::new(created.session_id)).block_task().await?;
            }
            Ok(())
        })).await.unwrap().unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), server_task)
        .await
        .unwrap()
        .unwrap();
    if decision == Decision::Disconnect {
        // The transport may observe EOF or a broken pipe while flushing the
        // cancelled prompt. Both must still clean up without running the tool.
        assert!(matches!(result, Ok(()) | Err(crate::AcpError::Protocol(_))));
    } else {
        result.unwrap();
    }
    let allowed = matches!(decision, Decision::Allow | Decision::Disabled);
    assert_eq!(
        tools.completions(),
        if allowed {
            vec!["approved-input".to_string()]
        } else {
            vec![]
        }
    );
    assert!(manager.sessions().is_empty());
    if decision != Decision::Disconnect {
        let updates = updates.lock().unwrap();
        assert!(updates.iter().any(|notification| matches!(&notification.update, acp::SessionUpdate::ToolCallUpdate(call) if call.fields.status == Some(if allowed { acp::ToolCallStatus::Completed } else { acp::ToolCallStatus::Failed }))));
        if !allowed {
            assert!(!updates.iter().any(|notification| matches!(&notification.update, acp::SessionUpdate::ToolCallUpdate(call) if call.fields.status == Some(acp::ToolCallStatus::InProgress))));
        }
    }
}

#[tokio::test]
async fn allow_executes_only_the_reviewed_arguments() {
    exercise(Decision::Allow).await;
}
#[tokio::test]
async fn reject_does_not_execute() {
    exercise(Decision::Reject).await;
}
#[tokio::test]
async fn unoffered_option_does_not_execute() {
    exercise(Decision::Invalid).await;
}
#[tokio::test]
async fn client_cancelled_outcome_does_not_execute() {
    exercise(Decision::Cancelled).await;
}
#[tokio::test]
async fn turn_cancel_ignores_late_allow() {
    exercise(Decision::CancelTurn).await;
}
#[tokio::test]
async fn session_close_ignores_late_allow() {
    exercise(Decision::Close).await;
}
#[tokio::test]
async fn timeout_ignores_late_allow() {
    exercise(Decision::Timeout).await;
}
#[tokio::test]
async fn protocol_error_does_not_execute() {
    exercise(Decision::Error).await;
}
#[tokio::test]
async fn disconnect_does_not_execute() {
    exercise(Decision::Disconnect).await;
}
#[tokio::test]
async fn disabled_permissions_preserve_existing_execution() {
    exercise(Decision::Disabled).await;
}

#[tokio::test]
async fn permission_channels_are_isolated_and_survive_reload_and_load() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path().canonicalize().unwrap();
    let tools = TestToolsPlugin::new();
    let manager = manager(tools.clone());
    let server = AcpServer::new(
        manager.clone(),
        AcpOptions::new(directory.path()).request_tool_permissions(Duration::from_secs(5)),
    );
    let (requests, mut pending) = mpsc::unbounded_channel();
    let (client_writer, server_reader) = tokio::io::duplex(64 * 1024);
    let (server_writer, client_reader) = tokio::io::duplex(64 * 1024);
    let server_task = tokio::spawn(server.serve(ByteStreams::new(
        server_writer.compat_write(),
        server_reader.compat(),
    )));
    tokio::time::timeout(
        Duration::from_secs(10),
        Client
            .builder()
            .on_receive_request(
                async move |request: acp::RequestPermissionRequest,
                            responder: Responder<acp::RequestPermissionResponse>,
                            _connection| {
                    requests.send((request, responder)).map_err(internal_error)
                },
                on_receive_request!(),
            )
            .connect_with(
                ByteStreams::new(client_writer.compat_write(), client_reader.compat()),
                async |connection| {
                    connection
                        .send_request(acp::InitializeRequest::new(
                            agent_client_protocol::schema::ProtocolVersion::V1,
                        ))
                        .block_task()
                        .await?;
                    let a = connection
                        .send_request(acp::NewSessionRequest::new(cwd.clone()))
                        .block_task()
                        .await?
                        .session_id;
                    let b = connection
                        .send_request(acp::NewSessionRequest::new(cwd.clone()))
                        .block_task()
                        .await?
                        .session_id;
                    let prompt = |id: acp::SessionId| {
                        acp::PromptRequest::new(
                            id,
                            vec![acp::ContentBlock::Text(acp::TextContent::new("act"))],
                        )
                    };
                    let selected = |id| {
                        acp::RequestPermissionResponse::new(
                            acp::RequestPermissionOutcome::Selected(
                                acp::SelectedPermissionOutcome::new(id),
                            ),
                        )
                    };
                    let first = connection.send_request(prompt(a.clone())).block_task();
                    let second = connection.send_request(prompt(b.clone())).block_task();
                    let (request_a, response_a) = pending.recv().await.unwrap();
                    let (request_b, response_b) = pending.recv().await.unwrap();
                    assert_ne!(request_a.session_id, request_b.session_id);
                    assert_eq!(
                        request_a.tool_call.tool_call_id,
                        request_b.tool_call.tool_call_id
                    );
                    assert!(tools.completions().is_empty());
                    for (request, responder) in [(request_a, response_a), (request_b, response_b)] {
                        assert!(request.session_id == a || request.session_id == b);
                        responder.respond(selected(if request.session_id == a {
                            REJECT_ONCE
                        } else {
                            ALLOW_ONCE
                        }))?;
                    }
                    first.await?;
                    second.await?;
                    assert_eq!(tools.completions().len(), 1);

                    // Reload reconstructs the plugin while retaining this ACP session's
                    // request channel. A prior decision never grants a later call.
                    let session = manager
                        .sessions()
                        .into_iter()
                        .find(|session| session.id() == b.0.as_ref())
                        .unwrap();
                    session.reload().await.map_err(internal_error)?;
                    let reloaded = connection.send_request(prompt(b.clone())).block_task();
                    let (request, responder) = pending.recv().await.unwrap();
                    assert_eq!(request.session_id, b);
                    assert_eq!(tools.completions().len(), 1);
                    responder.respond(selected(REJECT_ONCE))?;
                    reloaded.await?;

                    // Reopening a persisted session constructs a new transient channel.
                    connection
                        .send_request(acp::CloseSessionRequest::new(a.clone()))
                        .block_task()
                        .await?;
                    connection
                        .send_request(acp::LoadSessionRequest::new(a.clone(), cwd.clone()))
                        .block_task()
                        .await?;
                    let loaded = connection.send_request(prompt(a.clone())).block_task();
                    let (request, responder) = pending.recv().await.unwrap();
                    assert_eq!(request.session_id, a);
                    assert_eq!(tools.completions().len(), 1);
                    responder.respond(selected(ALLOW_ONCE))?;
                    loaded.await?;
                    connection
                        .send_request(acp::CloseSessionRequest::new(a))
                        .block_task()
                        .await?;
                    connection
                        .send_request(acp::CloseSessionRequest::new(b))
                        .block_task()
                        .await?;
                    Ok(())
                },
            ),
    )
    .await
    .unwrap()
    .unwrap();
    server_task.await.unwrap().unwrap();
    assert_eq!(
        tools.completions(),
        vec!["approved-input", "approved-input"]
    );
    assert!(manager.sessions().is_empty());
}
