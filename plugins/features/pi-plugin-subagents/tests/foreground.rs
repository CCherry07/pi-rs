use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_agent::AgentOptions;
use pi_core::{ModelId, PluginContext, PresentationMode, ProviderId, ToolCallId};
use pi_plugin_find::FindPlugin;
use pi_plugin_grep::GrepPlugin;
use pi_plugin_ls::LsPlugin;
use pi_plugin_read::ReadPlugin;
use pi_plugin_subagents::{
    SubagentLoaderOptions, SubagentRuntime, SubagentsPlugin, SubagentsSessionPlugin,
};
use pi_runtime::PiRuntime;
use pi_session::{
    AgentSession, AgentSessionOptions, AgentSessionRuntimeFactory, AgentSessionRuntimeRequest,
    AgentSessionRuntimeTarget, MultiSessionManager, PiPluginContext, PluginContextBinding,
    PreparedAgentSession, SessionError, SessionPlugins,
};
use pi_test_support::{ScriptedProvider, ScriptedProviderPlugin, ScriptedTurn};
use serde_json::{Value, json};

type RecordedProviders = Arc<Mutex<Vec<(usize, Arc<ScriptedProvider>)>>>;

#[derive(Clone)]
struct TestFactory {
    subagents: SubagentRuntime,
    binding: PluginContextBinding,
    providers: RecordedProviders,
    child_turns: Vec<ScriptedTurn>,
    agent_paths: Vec<PathBuf>,
}

impl TestFactory {
    fn new(child_turns: impl IntoIterator<Item = ScriptedTurn>) -> Self {
        Self {
            subagents: SubagentRuntime::default(),
            binding: PluginContextBinding::new(),
            providers: Arc::new(Mutex::new(Vec::new())),
            child_turns: child_turns.into_iter().collect(),
            agent_paths: Vec::new(),
        }
    }
}

#[async_trait]
impl AgentSessionRuntimeFactory for TestFactory {
    fn session_registered(&self, session: &pi_session::PiSession) {
        self.binding.bind(session.clone());
    }

    async fn prepare(
        &self,
        request: AgentSessionRuntimeRequest,
    ) -> Result<PreparedAgentSession, SessionError> {
        let initial_state = request.initial_state;
        let (cwd, path, restored_log) = match request.target {
            AgentSessionRuntimeTarget::Create { cwd, path, .. } => (cwd, path, None),
            AgentSessionRuntimeTarget::Open { path } => {
                let (log, document) = pi_session::SessionLog::open(&path)?;
                (document.header.cwd, path, Some(log))
            }
            AgentSessionRuntimeTarget::Reuse { log } => {
                let cwd = log.load()?.header.cwd;
                (cwd, log.path().to_path_buf(), Some(log))
            }
        };
        let depth = path
            .components()
            .filter(|component| component.as_os_str() == "isolated")
            .count();
        let turns = if depth == 0 {
            Vec::new()
        } else {
            self.child_turns.clone()
        };
        let provider_plugin = ScriptedProviderPlugin::scripted(turns);
        self.providers
            .lock()
            .unwrap()
            .push((depth, provider_plugin.provider()));
        let plugin_context = Arc::new(PiPluginContext::new(
            PresentationMode::Print,
            false,
            self.binding.clone(),
        ));
        let context_access: Arc<dyn PluginContext> = plugin_context.clone();
        let mut loader_options = SubagentLoaderOptions::new(&cwd, &cwd);
        loader_options.additional_paths = self.agent_paths.clone();
        let subagents = SubagentsPlugin::load(self.subagents.clone(), loader_options)
            .map_err(|error| SessionError::Runtime(error.to_string()))?;
        let (provider_id, model_id, thinking_level, active_tools) =
            initial_state.as_ref().map_or_else(
                || {
                    (
                        ProviderId::new("scripted"),
                        ModelId::new("test"),
                        pi_core::ThinkingLevel::Off,
                        [
                            "read",
                            "grep",
                            "find",
                            "ls",
                            "spawn_agent",
                            "send_message",
                            "followup_task",
                            "wait_agent",
                            "interrupt_agent",
                            "list_agents",
                        ]
                        .map(str::to_string)
                        .to_vec(),
                    )
                },
                |state| {
                    (
                        state.model.provider.clone(),
                        state.model.model_id.clone(),
                        state.thinking_level,
                        state.active_tools.clone(),
                    )
                },
            );
        let runtime = request
            .generation_overlay
            .apply_to(PiRuntime::builder())
            .plugin_context(context_access)
            .provider_plugin(provider_plugin)
            .agent_plugin(subagents)
            .agent_plugin(ReadPlugin)
            .agent_plugin(GrepPlugin)
            .agent_plugin(FindPlugin)
            .agent_plugin(LsPlugin)
            .agent_options(AgentOptions {
                provider_id,
                model_id,
                thinking_level,
                active_tools,
                cwd,
                ..AgentOptions::default()
            })
            .build()?;
        let options = AgentSessionOptions::default().plugins(
            SessionPlugins::new().plugin(SubagentsSessionPlugin::new(self.subagents.clone())),
        );
        let prepared = if let Some(log) = restored_log {
            AgentSession::prepare_reuse_with_options(runtime, log, options).await?
        } else {
            AgentSession::prepare_create_with_options(runtime, path, options).await?
        };
        plugin_context.bind_generation_session(prepared.session());
        Ok(prepared)
    }
}

async fn invoke(root: &pi_session::PiSession, name: &str, input: Value) -> pi_core::ToolResult {
    let session = root.current();
    let tool = session
        .runtime()
        .agent()
        .runtime()
        .registries()
        .tool(name)
        .unwrap_or_else(|| panic!("missing tool {name}"));
    let (_, signal) = pi_core::AbortHandle::new();
    let context = pi_core::ToolContext::with_plugin_context(
        root.cwd(),
        signal,
        session.runtime().context_parts(),
    );
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        tool.execute(
            context,
            ToolCallId::new(format!("test-{name}")),
            input,
            pi_core::ToolUpdateSink::channel().0,
        ),
    )
    .await
    .unwrap_or_else(|_| panic!("tool {name} must settle"))
    .unwrap()
}

async fn root(
    factory: TestFactory,
    directory: &tempfile::TempDir,
) -> (MultiSessionManager, pi_session::PiSession) {
    let manager = MultiSessionManager::new(factory);
    let root = manager
        .create_session(directory.path(), directory.path().join("parent.jsonl"))
        .await
        .unwrap();
    (manager, root)
}

#[tokio::test]
async fn spawn_wait_message_and_followup_reuse_one_agent_session() {
    let directory = tempfile::tempdir().unwrap();
    let factory = TestFactory::new([
        ScriptedTurn::Text("first review".into()),
        ScriptedTurn::Text("second review".into()),
    ]);
    let providers = factory.providers.clone();
    let (manager, root) = root(factory, &directory).await;

    let spawn = invoke(
        &root,
        "spawn_agent",
        json!({"agent":"reviewer","task":"review once"}),
    )
    .await;
    let details = spawn.details.as_ref().unwrap();
    let agent_id = details["agentId"].as_str().unwrap().to_string();
    let isolated_session_id = details["isolatedSessionId"].as_str().unwrap().to_string();
    let spawned_session_id = details["sessionId"].as_str().unwrap().to_string();
    assert!(!agent_id.is_empty());
    assert!(!isolated_session_id.is_empty());

    let first = invoke(
        &root,
        "wait_agent",
        json!({"targets":[agent_id],"mode":"all","timeoutMs":2000}),
    )
    .await;
    assert_eq!(
        first.details.as_ref().unwrap()["agents"][0]["state"],
        "idle"
    );
    assert_eq!(
        first.details.as_ref().unwrap()["agents"][0]["lastResult"]["text"],
        "first review"
    );
    let child_session_id = first.details.as_ref().unwrap()["agents"][0]["childSessionId"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(spawned_session_id, child_session_id);

    let sent = invoke(
        &root,
        "send_message",
        json!({"target":agent_id,"message":"preserve this context"}),
    )
    .await;
    assert_eq!(sent.details.as_ref().unwrap()["acceptedAs"], "mailbox");
    let follow = invoke(
        &root,
        "followup_task",
        json!({"target":agent_id,"task":"review again"}),
    )
    .await;
    assert_eq!(follow.details.as_ref().unwrap()["started"], true);
    let second = invoke(
        &root,
        "wait_agent",
        json!({"targets":[agent_id],"mode":"all","timeoutMs":2000}),
    )
    .await;
    assert_eq!(
        second.details.as_ref().unwrap()["agents"][0]["lastResult"]["text"],
        "second review"
    );
    assert_eq!(
        second.details.as_ref().unwrap()["agents"][0]["childSessionId"],
        child_session_id
    );
    let child_provider = providers
        .lock()
        .unwrap()
        .iter()
        .find(|(depth, _)| *depth == 1)
        .unwrap()
        .1
        .clone();
    assert_eq!(child_provider.requests().len(), 2);
    assert!(
        format!("{:?}", child_provider.requests()[1].messages).contains("preserve this context")
    );

    let list = invoke(&root, "list_agents", json!({})).await;
    assert_eq!(
        list.details.as_ref().unwrap()["agents"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn interrupt_stops_only_the_active_turn_and_agent_remains_reusable() {
    let directory = tempfile::tempdir().unwrap();
    let factory = TestFactory::new([
        ScriptedTurn::WaitForAbort,
        ScriptedTurn::WaitForAbort,
        ScriptedTurn::Text("recovered follow-up".into()),
    ]);
    let (manager, root) = root(factory, &directory).await;
    let spawn = invoke(
        &root,
        "spawn_agent",
        json!({"agent":"reviewer","task":"wait"}),
    )
    .await;
    let agent_id = spawn.details.as_ref().unwrap()["agentId"]
        .as_str()
        .unwrap()
        .to_string();

    let sent = invoke(
        &root,
        "send_message",
        json!({"target":agent_id,"message":"steer while active"}),
    )
    .await;
    assert_eq!(sent.details.as_ref().unwrap()["acceptedAs"], "steer");
    invoke(&root, "interrupt_agent", json!({"target":agent_id})).await;
    let interrupted = invoke(
        &root,
        "wait_agent",
        json!({"targets":[agent_id],"timeoutMs":2000}),
    )
    .await;
    assert_eq!(
        interrupted.details.as_ref().unwrap()["agents"][0]["state"],
        "interrupted"
    );

    let follow = invoke(
        &root,
        "followup_task",
        json!({"target":agent_id,"task":"continue"}),
    )
    .await;
    assert_eq!(follow.details.as_ref().unwrap()["started"], true);
    let followup_message = invoke(
        &root,
        "send_message",
        json!({"target":agent_id,"message":"steer the follow-up"}),
    )
    .await;
    assert_eq!(
        followup_message.details.as_ref().unwrap()["acceptedAs"],
        "steer"
    );
    invoke(&root, "interrupt_agent", json!({"target":agent_id})).await;
    let second_interrupted = invoke(
        &root,
        "wait_agent",
        json!({"targets":[agent_id],"timeoutMs":2000}),
    )
    .await;
    assert_eq!(
        second_interrupted.details.as_ref().unwrap()["agents"][0]["state"],
        "interrupted"
    );
    let final_follow = invoke(
        &root,
        "followup_task",
        json!({"target":agent_id,"task":"finish"}),
    )
    .await;
    assert_eq!(final_follow.details.as_ref().unwrap()["started"], true);
    let recovered = invoke(
        &root,
        "wait_agent",
        json!({"targets":[agent_id],"timeoutMs":2000}),
    )
    .await;
    assert_eq!(
        recovered.details.as_ref().unwrap()["agents"][0]["state"],
        "idle"
    );
    assert_eq!(
        recovered.details.as_ref().unwrap()["agents"][0]["lastResult"]["text"],
        "recovered follow-up"
    );
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn wait_any_is_a_race_and_wait_all_is_a_barrier() {
    let directory = tempfile::tempdir().unwrap();
    let (manager, root) = root(TestFactory::new([ScriptedTurn::WaitForAbort]), &directory).await;
    let (left, right) = tokio::join!(
        invoke(
            &root,
            "spawn_agent",
            json!({"agent":"reviewer","task":"left"}),
        ),
        invoke(
            &root,
            "spawn_agent",
            json!({"agent":"reviewer","task":"right"}),
        )
    );
    let left_id = left.details.as_ref().unwrap()["agentId"]
        .as_str()
        .unwrap()
        .to_string();
    let right_id = right.details.as_ref().unwrap()["agentId"]
        .as_str()
        .unwrap()
        .to_string();

    invoke(&root, "interrupt_agent", json!({"target":left_id})).await;
    let raced = invoke(
        &root,
        "wait_agent",
        json!({"targets":[left_id,right_id],"mode":"any","timeoutMs":2000}),
    )
    .await;
    assert_eq!(raced.details.as_ref().unwrap()["state"], "ready");
    let states = raced.details.as_ref().unwrap()["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|agent| agent["state"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(states.contains(&"interrupted"));
    assert!(states.contains(&"running"));

    invoke(&root, "interrupt_agent", json!({"target":right_id})).await;
    let barrier = invoke(
        &root,
        "wait_agent",
        json!({"targets":[left_id,right_id],"mode":"all","timeoutMs":2000}),
    )
    .await;
    assert_eq!(barrier.details.as_ref().unwrap()["state"], "ready");
    assert!(
        barrier.details.as_ref().unwrap()["agents"]
            .as_array()
            .unwrap()
            .iter()
            .all(|agent| agent["state"] == "interrupted")
    );
    manager.shutdown().await.unwrap();
}

#[test]
fn old_workflow_and_supervisor_tools_are_not_registered() {
    let runtime = PiRuntime::builder()
        .agent_plugin(SubagentsPlugin::default())
        .build()
        .unwrap();
    for old in [
        "subagent",
        "subagent_workflow",
        "contact_supervisor",
        "subagent_supervisor",
        "bg_wait",
    ] {
        assert!(
            runtime.tool_specs().iter().all(|tool| tool.name != old),
            "{old}"
        );
    }
}
