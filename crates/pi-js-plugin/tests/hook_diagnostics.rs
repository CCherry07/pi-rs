//! Error classification and source identity at the adapter/driver handoff.
//! Oracle: legacy ExtensionRunner emitToolCall, emitUserBash and boundary dispatch.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_core::{AbortHandle, AgentContext, AssistantMessage, RunId, ToolCall};
use pi_js_plugin::{
    JsAgentPluginManifest, JsCallbackDispatcher, JsCallbackError, JsGenerationManifest,
    JsHookSnapshot, JsInvocation, JsPluginGeneration,
};
use pi_plugin::{
    AgentActivityOutcome, BoundaryContextPreview, PluginContextHandle, PluginDriver,
    RegistriesBuilder, SessionDispatchContext, SessionIdentity, ToolCallEvent, UserBashEvent,
};
use serde_json::{Value, json};

struct Dispatcher {
    hook: &'static str,
    invalid: Option<Value>,
    calls: Mutex<Vec<String>>,
}

#[async_trait]
impl JsCallbackDispatcher for Dispatcher {
    async fn snapshot_hooks(
        &self,
        _: &str,
        hook: &str,
    ) -> Result<Option<Vec<JsHookSnapshot>>, JsCallbackError> {
        assert_eq!(hook, self.hook);
        Ok(Some(
            ["bad", "next"]
                .map(|name| JsHookSnapshot {
                    plugin_id: name.into(),
                    callback_id: name.into(),
                    source_path: Some(format!("/extensions/{name}.ts")),
                })
                .into(),
        ))
    }

    async fn invoke(
        &self,
        invocation: JsInvocation,
        _: PluginContextHandle,
    ) -> Result<Value, JsCallbackError> {
        self.calls
            .lock()
            .unwrap()
            .push(invocation.callback_id.clone());
        assert_eq!(
            invocation.payload["context"]["pluginId"],
            invocation.callback_id
        );
        match invocation.callback_id.as_str() {
            "bad" => self
                .invalid
                .clone()
                .ok_or_else(|| JsCallbackError::new("fixture boom")),
            "next" => Ok(json!({"continue":false})),
            other => panic!("unexpected callback {other}"),
        }
    }
}

fn driver(hook: &'static str, invalid: Option<Value>) -> (PluginDriver, Arc<Dispatcher>) {
    let dispatcher = Arc::new(Dispatcher {
        hook,
        invalid,
        calls: Mutex::new(Vec::new()),
    });
    let generation = JsPluginGeneration::prepare(
        JsGenerationManifest {
            generation_id: "diagnostics".into(),
            dynamic_hooks: true,
            agent_plugins: vec![JsAgentPluginManifest {
                id: "coordinator".into(),
                tools: vec![],
                commands: vec![],
                hooks: vec![],
            }],
            provider_plugins: vec![],
            provider_registrations: vec![],
            session_plugins: vec![],
            diagnostics: vec![],
        },
        dispatcher.clone(),
    )
    .unwrap();
    let (driver, _) = RegistriesBuilder::new()
        .register_plugins(generation.plugins())
        .unwrap();
    (driver, dispatcher)
}

fn assert_one_diagnostic(driver: &PluginDriver, hook: &str, prefix: &str) {
    let diagnostics = driver.diagnostics();
    assert_eq!(diagnostics.len(), 1);
    let diagnostic = &diagnostics[0];
    assert_eq!(diagnostic.plugin_id.as_str(), "bad");
    assert_eq!(
        diagnostic.source_path.as_deref(),
        Some("/extensions/bad.ts")
    );
    assert_eq!(diagnostic.hook.as_str(), hook);
    assert!(
        diagnostic
            .raw_message
            .as_deref()
            .unwrap()
            .starts_with(prefix)
    );
    assert!(
        diagnostic
            .message
            .contains(&format!("bad failed in {hook}")),
        "non-RPC display keeps context"
    );
}

#[tokio::test]
async fn boundary_throw_and_invalid_patch_report_once_then_continue_with_the_right_source() {
    for invalid in [None, Some(json!({"continue":"invalid"}))] {
        let prefix = if invalid.is_some() {
            "invalid agent_before_settle result:"
        } else {
            "fixture boom"
        };
        let (driver, dispatcher) = driver("agent_before_settle", invalid);
        let observed = Arc::new(Mutex::new(Vec::new()));
        let captured = observed.clone();
        let _subscription = driver.observe_diagnostics(Arc::new(move |diagnostic| {
            captured.lock().unwrap().push(diagnostic.clone())
        }));
        let (_, signal) = AbortHandle::new();
        let result = driver
            .dispatch_boundary(
                &RunId::new("run"),
                std::path::Path::new("/workspace"),
                &signal,
                None,
                AgentActivityOutcome::Completed,
                |_| Ok(BoundaryContextPreview::default()),
            )
            .await;
        assert_eq!(result.continue_run, Some(false));
        assert_eq!(*dispatcher.calls.lock().unwrap(), ["bad", "next"]);
        assert_one_diagnostic(&driver, "agent_before_settle", prefix);
        assert_eq!(observed.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn user_bash_throw_and_invalid_patch_report_once_and_never_fall_back_to_local_execution() {
    for invalid in [None, Some(json!({"result":{"output":42}}))] {
        let prefix = if invalid.is_some() {
            "invalid JavaScript session hook result:"
        } else {
            "fixture boom"
        };
        let (driver, dispatcher) = driver("user_bash", invalid);
        let context = SessionDispatchContext {
            generation: 7,
            identity: SessionIdentity {
                id: "session".into(),
                path: "/sessions/session.jsonl".into(),
                cwd: "/workspace".into(),
                parent_session_id: None,
            },
        };
        let (_, signal) = AbortHandle::new();
        let error = driver
            .user_bash(
                &context,
                &UserBashEvent {
                    command: "must not execute".into(),
                    cwd: "/workspace".into(),
                    exclude_from_context: false,
                    signal,
                },
            )
            .await
            .err()
            .expect("must fail closed");
        assert!(error.diagnostic_message().starts_with(prefix));
        assert_eq!(*dispatcher.calls.lock().unwrap(), ["bad"]);
        assert_one_diagnostic(&driver, "user_bash", prefix);
        assert_eq!(driver.diagnostics()[0].generation, Some(7));
    }
}

#[tokio::test]
async fn tool_call_failure_propagates_without_an_extra_extension_error() {
    let (driver, dispatcher) = driver("tool_call", None);
    let (_, signal) = AbortHandle::new();
    let assistant_message: AssistantMessage = serde_json::from_value(json!({
        "role":"assistant", "content":[], "api":"test", "provider":"test", "model":"test",
        "usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},
        "stopReason":"toolUse", "timestamp":1
    })).unwrap();
    let error = driver
        .tool_call(
            &RunId::new("run"),
            std::path::Path::new("/workspace"),
            &signal,
            ToolCallEvent {
                assistant_message,
                tool_call: ToolCall::new("call", "tool", json!({})),
                validated_args: json!({}),
                context: Arc::new(AgentContext {
                    system_prompt: String::new(),
                    messages: vec![],
                    active_tools: vec!["tool".into()],
                }),
                parent_tool_call_id: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.diagnostic_message(), "fixture boom");
    assert!(
        matches!(error, pi_plugin::PluginError::Hook { ref plugin_id, source_path: Some(ref path), .. }
        if plugin_id.as_str() == "bad" && path == "/extensions/bad.ts")
    );
    assert_eq!(*dispatcher.calls.lock().unwrap(), ["bad"]);
    assert!(
        driver.diagnostics().is_empty(),
        "legacy emitToolCall only propagates to the tool failure"
    );
}
