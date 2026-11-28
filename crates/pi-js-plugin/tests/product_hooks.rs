//! Session product hooks preserve the Pi extension contract across the Rust/JS adapter.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_core::{AbortHandle, ModelSpec};
use pi_js_plugin::{
    JsCallbackDispatcher, JsCallbackError, JsGenerationManifest, JsHookManifest, JsInvocation,
    JsInvocationKind, JsPluginGeneration, JsSessionPluginManifest,
};
use pi_plugin::{
    BashOutputSink, ModelSelectEvent, ModelSelectSource, PluginContextHandle, PluginDriver,
    RegistriesBuilder, SessionDispatchContext, SessionIdentity, ThinkingLevelSelectEvent,
    UserBashEvent, UserBashOverride, UserBashRequest,
};
use serde_json::{Value, json};

#[derive(Default)]
struct Dispatcher {
    replies: Mutex<VecDeque<Result<Value, JsCallbackError>>>,
    invocations: Mutex<Vec<JsInvocation>>,
    cancelled: Mutex<Vec<String>>,
    pending_operations: bool,
    operations_started: tokio::sync::Notify,
}

#[async_trait]
impl JsCallbackDispatcher for Dispatcher {
    async fn invoke(
        &self,
        invocation: JsInvocation,
        _context: PluginContextHandle,
    ) -> Result<Value, JsCallbackError> {
        self.invocations.lock().unwrap().push(invocation);
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected callback")
    }

    async fn invoke_with_bash_output(
        &self,
        invocation: JsInvocation,
        _context: PluginContextHandle,
        output: BashOutputSink,
    ) -> Result<Value, JsCallbackError> {
        assert!(matches!(invocation.kind, JsInvocationKind::UserBash));
        assert_eq!(
            invocation.payload,
            json!({"command":"remote", "cwd":"/remote"})
        );
        self.invocations.lock().unwrap().push(invocation);
        output("远程 output".as_bytes().to_vec());
        self.operations_started.notify_one();
        if self.pending_operations {
            std::future::pending::<()>().await;
        }
        Ok(json!({"exitCode":17}))
    }

    fn cancel(&self, invocation_id: &str) {
        self.cancelled.lock().unwrap().push(invocation_id.into());
    }
}

fn driver(hooks: &[&str], dispatcher: Arc<Dispatcher>) -> PluginDriver {
    let generation = JsPluginGeneration::prepare(
        JsGenerationManifest {
            generation_id: "product-hooks".into(),
            dynamic_hooks: false,
            agent_plugins: Vec::new(),
            provider_plugins: Vec::new(),
            provider_registrations: Vec::new(),
            mcp_server_registrations: Vec::new(),
            session_plugins: vec![JsSessionPluginManifest {
                id: "extension".into(),
                hooks: hooks
                    .iter()
                    .enumerate()
                    .map(|(index, name)| JsHookManifest {
                        name: (*name).into(),
                        callback_id: format!("callback-{index}"),
                    })
                    .collect(),
            }],
            diagnostics: Vec::new(),
        },
        dispatcher,
    )
    .unwrap();
    RegistriesBuilder::new()
        .register_plugins(generation.plugins())
        .unwrap()
        .0
}

fn context() -> SessionDispatchContext {
    SessionDispatchContext {
        generation: 1,
        identity: SessionIdentity {
            id: "session".into(),
            path: "/sessions/session.jsonl".into(),
            cwd: "/remote".into(),
            parent_session_id: None,
        },
    }
}

#[tokio::test]
async fn model_select_preserves_previous_model_source_and_observer_order() {
    let dispatcher = Arc::new(Dispatcher {
        replies: Mutex::new(VecDeque::from([
            Ok(json!({"cancel":true})),
            Err(JsCallbackError::new("observer failure")),
            Ok(Value::Null),
        ])),
        ..Default::default()
    });
    let driver = driver(
        &["model_select", "model_select", "model_select"],
        dispatcher.clone(),
    );
    let model = ModelSpec::new("provider", "new", "New", "api");
    let previous = ModelSpec::new("provider", "old", "Old", "api");
    driver
        .model_select(
            &context(),
            &ModelSelectEvent {
                model: model.clone(),
                previous_model: Some(previous.clone()),
                source: ModelSelectSource::Cycle,
            },
        )
        .await;
    let invocations = dispatcher.invocations.lock().unwrap();
    assert_eq!(
        invocations.len(),
        3,
        "observer return values and errors cannot stop following observers"
    );
    assert_eq!(
        invocations[0].payload["event"],
        json!({
            "type":"model_select", "model":model, "previousModel":previous, "source":"cycle",
        })
    );
    assert!(
        driver
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.message.contains("observer failure"))
    );
}

#[tokio::test]
async fn thinking_level_select_uses_the_pi_payload() {
    let dispatcher = Arc::new(Dispatcher {
        replies: Mutex::new(VecDeque::from([Ok(Value::Null)])),
        ..Default::default()
    });
    let driver = driver(&["thinking_level_select"], dispatcher.clone());
    driver
        .thinking_level_select(
            &context(),
            &ThinkingLevelSelectEvent {
                level: pi_core::ThinkingLevel::High,
                previous_level: pi_core::ThinkingLevel::Low,
            },
        )
        .await;
    assert_eq!(
        dispatcher.invocations.lock().unwrap()[0].payload["event"],
        json!({
            "type":"thinking_level_select", "level":"high", "previousLevel":"low",
        })
    );
}

#[tokio::test]
async fn user_bash_stops_at_first_result_and_reports_callback_errors() {
    for fail in [false, true] {
        let reply = if fail {
            Err(JsCallbackError::new("shell hook failed"))
        } else {
            Ok(json!({
                "result": {"output":"remote output", "exitCode":4, "cancelled":false, "truncated":false},
            }))
        };
        let dispatcher = Arc::new(Dispatcher {
            replies: Mutex::new(VecDeque::from([Ok(Value::Null), reply])),
            ..Default::default()
        });
        let driver = driver(&["user_bash", "user_bash", "user_bash"], dispatcher.clone());
        let (_, signal) = AbortHandle::new();
        let result = driver
            .user_bash(
                &context(),
                &UserBashEvent {
                    command: "remote".into(),
                    cwd: "/remote".into(),
                    exclude_from_context: true,
                    signal,
                },
            )
            .await;
        if fail {
            assert!(
                result
                    .err()
                    .expect("expected callback failure")
                    .to_string()
                    .contains("shell hook failed")
            );
        } else {
            let Some(UserBashOverride::Result(result)) = result.unwrap() else {
                panic!("expected complete result")
            };
            assert_eq!(result.output, "remote output");
            assert_eq!(result.exit_code, Some(4));
        }
        let invocations = dispatcher.invocations.lock().unwrap();
        assert_eq!(invocations.len(), 2);
        assert_eq!(
            invocations[0].payload["event"],
            json!({
                "type":"user_bash", "command":"remote", "cwd":"/remote", "excludeFromContext":true,
            })
        );
    }
}

#[tokio::test]
async fn user_bash_operations_forward_bytes_and_cancel_inflight_js_invocations() {
    for pending_operations in [false, true] {
        let dispatcher = Arc::new(Dispatcher {
            replies: Mutex::new(VecDeque::from([Ok(
                json!({"operationsCallbackId":"remote-exec"}),
            )])),
            pending_operations,
            ..Default::default()
        });
        let driver = driver(&["user_bash"], dispatcher.clone());
        let (abort, signal) = AbortHandle::new();
        let result = driver
            .user_bash(
                &context(),
                &UserBashEvent {
                    command: "remote".into(),
                    cwd: "/remote".into(),
                    exclude_from_context: false,
                    signal: signal.clone(),
                },
            )
            .await
            .unwrap();
        let Some(UserBashOverride::Operations(operations)) = result else {
            panic!("expected custom operations")
        };
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let captured = bytes.clone();
        let execution = tokio::spawn(async move {
            operations
                .execute(UserBashRequest {
                    command: "remote".into(),
                    cwd: "/remote".into(),
                    signal,
                    on_data: Arc::new(move |chunk| captured.lock().unwrap().extend(chunk)),
                })
                .await
        });
        dispatcher.operations_started.notified().await;
        if pending_operations {
            abort.abort();
        }
        let result = execution.await.unwrap();
        assert_eq!(*bytes.lock().unwrap(), "远程 output".as_bytes());
        if pending_operations {
            assert!(result.unwrap_err().to_string().contains("aborted"));
            let invocations = dispatcher.invocations.lock().unwrap();
            assert_eq!(
                *dispatcher.cancelled.lock().unwrap(),
                [invocations[1].invocation_id.clone()]
            );
        } else {
            assert_eq!(result.unwrap(), Some(17));
            assert!(dispatcher.cancelled.lock().unwrap().is_empty());
        }
    }
}
