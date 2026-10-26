use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_agent::AgentOptions;
use pi_core::{
    ToolCall, ToolCallId, ToolResult, ToolSpec, WorkspaceRoot, WorkspaceRootId, WorkspaceSpec,
};
use pi_plugin::{Plugin, PluginId, RegisterContext, Tool, ToolContext, ToolError, ToolUpdateSink};
use pi_runtime::PiRuntime;
use pi_session::{
    AgentSessionOptions, MultiSessionManager, PreparedSessionGeneration, SessionError,
    SessionGenerationRequest, SessionHeader, SessionStore,
};
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};
use serde_json::{Value, json};

#[derive(Clone)]
struct Probe(Arc<Mutex<Vec<(&'static str, WorkspaceSpec)>>>);

#[derive(Clone, Default, serde::Deserialize)]
struct ProbeOptions {
    #[serde(skip)]
    observations: Arc<Mutex<Vec<(&'static str, WorkspaceSpec)>>>,
}

#[pi_plugin::plugin]
impl Plugin for Probe {
    type Options = ProbeOptions;
    fn prepare(
        context: &pi_plugin::PrepareContext,
        options: ProbeOptions,
    ) -> Result<Option<Self>, pi_plugin::PrepareError> {
        options
            .observations
            .lock()
            .unwrap()
            .push(("factory", context.workspace().spec().clone()));
        Ok(Some(Self(options.observations)))
    }

    fn id(&self) -> PluginId {
        PluginId::new("workspace-probe")
    }
    fn register(&self, context: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_tool(Arc::new(self.clone()))
    }
    async fn before_agent_start(
        &self,
        context: pi_plugin::AgentPluginContext,
        _: pi_plugin::BeforeAgentStartEvent,
    ) -> Result<pi_plugin::BeforeAgentStartPatch, pi_plugin::PluginError> {
        self.0
            .lock()
            .unwrap()
            .push(("hook", context.workspace().spec().clone()));
        Ok(Default::default())
    }
    async fn session_start(
        &self,
        context: &pi_plugin::SessionPluginContext,
        _: &pi_plugin::SessionStartEvent,
    ) -> Result<(), pi_plugin::PluginError> {
        self.0
            .lock()
            .unwrap()
            .push(("session", context.workspace().spec().clone()));
        Ok(())
    }
}

#[async_trait]
impl Tool for Probe {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "probe".into(),
            label: "probe".into(),
            description: "Observe workspace".into(),
            parameters: json!({"type":"object"}),
            execution_mode: Default::default(),
            prompt_snippet: None,
            prompt_guidelines: vec![],
        }
    }
    async fn prepare_arguments(
        &self,
        context: &ToolContext,
        input: Value,
    ) -> Result<Value, ToolError> {
        self.0
            .lock()
            .unwrap()
            .push(("prepare", context.workspace().spec().clone()));
        Ok(input)
    }
    async fn execute(
        &self,
        context: ToolContext,
        _: ToolCallId,
        _: Value,
        _: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        assert_eq!(context.cwd(), context.workspace().cwd());
        self.0
            .lock()
            .unwrap()
            .push(("execute", context.workspace().spec().clone()));
        Ok(ToolResult::text("done"))
    }
}

fn workspace(path: &std::path::Path, name: &str) -> WorkspaceSpec {
    WorkspaceSpec::new(
        vec![
            WorkspaceRoot::external("primary", "primary", path),
            WorkspaceRoot::external(name, name, path.join(name)),
        ],
        WorkspaceRootId::new("primary"),
        path,
    )
    .unwrap()
}

#[tokio::test]
async fn multi_root_workspace_survives_execution_reload_resume_and_fork() {
    let root = tempfile::tempdir().unwrap();
    let probe = Probe(Arc::default());
    let recorded = probe.0.clone();
    let fail = Arc::new(AtomicBool::new(false));
    let failure = fail.clone();
    let manager = MultiSessionManager::new(move |request: SessionGenerationRequest| {
        let probe = probe.clone();
        let failure = failure.clone();
        async move {
            if failure.load(Ordering::SeqCst) {
                return Err(SessionError::Runtime("failed candidate".into()));
            }
            let runtime = request
                .generation_overlay
                .apply_to(PiRuntime::builder())
                .workspace(request.workspace)
                .agent_options(AgentOptions {
                    cwd: request.cwd,
                    active_tools: vec!["probe".into()],
                    ..Default::default()
                })
                .prepare_plugin::<Probe>(
                    "/agent-profile",
                    false,
                    ProbeOptions {
                        observations: probe.0,
                    },
                )
                .provider_plugin(ScriptedProviderPlugin::scripted([
                    ScriptedTurn::ToolCalls(vec![ToolCall {
                        id: ToolCallId::new("call"),
                        name: "probe".into(),
                        arguments: json!({}),
                        namespace: None,
                        thought_signature: None,
                    }]),
                    ScriptedTurn::Text("complete".into()),
                ]))
                .build()?;
            Ok(PreparedSessionGeneration::new(
                runtime,
                AgentSessionOptions::default(),
            ))
        }
    });
    let first_spec = workspace(root.path(), "shared-a");
    let second_spec = workspace(root.path(), "shared-b");
    let path = root.path().join("a.jsonl");
    let metadata = serde_json::Map::from_iter([("unrelated".into(), json!({"preserve":true}))]);
    let first = manager
        .create_session(pi_session::CreateSessionOptions {
            workspace: first_spec.clone(),
            path: path.clone(),
            session_id: Some("workspace-session".into()),
            metadata: Some(metadata),
            generation_overlay: pi_session::SessionGenerationOverlay::new()
                .with_execution_origin(pi_plugin::SessionExecutionOrigin::Subagent),
        })
        .await
        .unwrap();
    let second = manager
        .create_session(pi_session::CreateSessionOptions {
            workspace: second_spec.clone(),
            path: root.path().join("b.jsonl"),
            session_id: None,
            metadata: None,
            generation_overlay: Default::default(),
        })
        .await
        .unwrap();
    assert!(!path.exists());
    assert_eq!(first.id(), "workspace-session");
    assert_eq!(
        first.current().catalog().execution_origin(),
        pi_plugin::SessionExecutionOrigin::Subagent
    );
    first.reload().await.unwrap();
    assert!(!path.exists());
    assert_eq!(first.id(), "workspace-session");
    assert_eq!(
        first.current().catalog().execution_origin(),
        pi_plugin::SessionExecutionOrigin::Subagent
    );
    let first_current = first.current();
    let second_current = second.current();
    let (a, b) = tokio::join!(first_current.prompt("a"), second_current.prompt("b"));
    a.unwrap();
    b.unwrap();
    assert!(path.exists());
    assert_eq!(
        SessionStore::read(&path)
            .unwrap()
            .header
            .workspace()
            .unwrap(),
        first_spec
    );
    fail.store(true, Ordering::SeqCst);
    assert!(first.reload().await.is_err());
    assert_eq!(first.current().catalog().workspace().spec(), &first_spec);
    fail.store(false, Ordering::SeqCst);
    for stage in ["factory", "hook", "prepare", "execute", "session"] {
        let observed = recorded.lock().unwrap();
        assert!(
            observed
                .iter()
                .any(|(kind, spec)| *kind == stage && spec == &first_spec)
        );
        assert!(
            observed
                .iter()
                .any(|(kind, spec)| *kind == stage && spec == &second_spec)
        );
    }
    let fork_point = first.current().history().leaf_id().unwrap();
    first
        .fork_session(fork_point, pi_session::ForkPosition::At)
        .await
        .unwrap();
    assert_eq!(first.current().catalog().workspace().spec(), &first_spec);
    assert_eq!(
        first
            .current()
            .history()
            .header()
            .metadata
            .as_ref()
            .unwrap()["unrelated"],
        json!({"preserve":true})
    );
    first.resume_session(&path).await.unwrap();
    assert_eq!(first.current().catalog().workspace().spec(), &first_spec);
    manager.shutdown().await.unwrap();
}

#[test]
fn workspace_metadata_is_strict_and_preserves_other_extensions() {
    let spec = workspace(std::path::Path::new("/app"), "shared");
    let mut header = SessionHeader::new("session", "/app");
    assert_eq!(header.workspace().unwrap(), WorkspaceSpec::from_cwd("/app"));
    header.metadata = Some(serde_json::Map::from_iter([(
        "other".into(),
        json!([1, 2]),
    )]));
    header.set_workspace(&spec).unwrap();
    assert_eq!(header.workspace().unwrap(), spec);
    assert_eq!(header.metadata.as_ref().unwrap()["other"], json!([1, 2]));
    for bad in [
        json!(null),
        json!({"schemaVersion":2}),
        json!({"schemaVersion":1,"roots":[]}),
    ] {
        header
            .metadata
            .as_mut()
            .unwrap()
            .insert("pi-rs.workspace".into(), bad);
        assert!(header.workspace().is_err());
    }
    header.set_workspace(&spec).unwrap();
    header.cwd = "/elsewhere".into();
    assert!(header.workspace().is_err());
    let root = tempfile::tempdir().unwrap();
    assert!(SessionStore::create_deferred(root.path().join("bad.jsonl"), header).is_err());
}
