use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_agent::AgentOptions;
use pi_core::{
    ContentBlock, ModelId, ModelSpec, PluginContext, PluginId, PresentationMode, ProviderId,
    ProviderPlugin, ProviderRegisterContext, ToolCallId, ToolResult,
};
use pi_plugin_find::FindPlugin;
use pi_plugin_grep::GrepPlugin;
use pi_plugin_ls::LsPlugin;
use pi_plugin_read::ReadPlugin;
use pi_plugin_subagents::{
    SubagentLoaderOptions, SubagentRuntime, SubagentsPlugin, SubagentsSessionPlugin,
};
use pi_runtime::PiRuntime;
use pi_session::{
    AgentSessionOptions, MultiSessionManager, PiPluginContext, PluginContextBinding,
    PreparedSessionGeneration, SessionError, SessionGenerationFactory, SessionGenerationRequest,
    SessionPlugins,
};
use pi_test_support::{ScriptedProvider, ScriptedProviderPlugin, ScriptedTurn};
use serde_json::{Value, json};

type RecordedProviders = Arc<Mutex<Vec<(usize, Arc<ScriptedProvider>)>>>;

struct TestModelCatalogPlugin;

#[pi_core::provider_plugin]
impl ProviderPlugin for TestModelCatalogPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("test-model-catalog")
    }

    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_core::Result<()> {
        let mut model = ModelSpec::new("scripted", "test", "Test", "scripted");
        model.reasoning = true;
        context.register_model(model)
    }
}

#[derive(Clone)]
struct TestFactory {
    subagents: SubagentRuntime,
    binding: PluginContextBinding,
    providers: RecordedProviders,
    root_turns: Vec<ScriptedTurn>,
    child_turns: Vec<ScriptedTurn>,
    agent_paths: Vec<PathBuf>,
}

impl TestFactory {
    fn new(child_turns: impl IntoIterator<Item = ScriptedTurn>) -> Self {
        Self {
            subagents: SubagentRuntime::default(),
            binding: PluginContextBinding::new(),
            providers: Arc::new(Mutex::new(Vec::new())),
            root_turns: Vec::new(),
            child_turns: child_turns.into_iter().collect(),
            agent_paths: Vec::new(),
        }
    }

    fn with_root_turns(mut self, turns: impl IntoIterator<Item = ScriptedTurn>) -> Self {
        self.root_turns = turns.into_iter().collect();
        self
    }
}

#[async_trait]
impl SessionGenerationFactory for TestFactory {
    fn session_registered(&self, session: &pi_session::PiSession) {
        self.binding.bind(session.clone());
        self.subagents.session_registered(session.clone());
    }

    async fn prepare_generation(
        &self,
        request: SessionGenerationRequest,
    ) -> Result<PreparedSessionGeneration, SessionError> {
        let initial_state = request.initial_state;
        let cwd = request.cwd;
        let depth = request
            .session_path
            .components()
            .filter(|component| component.as_os_str() == "isolated")
            .count();
        let turns = if depth == 0 {
            self.root_turns.clone()
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
            .provider_plugin(TestModelCatalogPlugin)
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
        Ok(PreparedSessionGeneration::new(runtime, options)
            .bind_session(move |session| plugin_context.bind_generation_session(session)))
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
    .unwrap_or_else(|error| panic!("tool {name} failed: {error}"))
}

fn tool_text(result: &ToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
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

fn desktop_widget(root: &pi_session::PiSession) -> Value {
    root.current()
        .log()
        .load()
        .unwrap()
        .entries
        .iter()
        .rev()
        .find_map(|entry| {
            let value = serde_json::to_value(entry).unwrap();
            (value["customType"] == "pi.ui.widget" && value["data"]["key"] == "subagents.tasks")
                .then(|| value["data"]["value"].clone())
        })
        .expect("subagent widget snapshot")
}

fn joined_reports(root: &pi_session::PiSession) -> Vec<Value> {
    root.current()
        .log()
        .load()
        .unwrap()
        .entries
        .iter()
        .filter_map(|entry| {
            let value = serde_json::to_value(entry).unwrap();
            if value["type"] == "custom_message" && value["customType"] == "agent_settled" {
                Some(value)
            } else if value["type"] == "message"
                && value["message"]["role"] == "custom"
                && value["message"]["customType"] == "agent_settled"
            {
                Some(value["message"].clone())
            } else {
                None
            }
        })
        .collect()
}

async fn wait_for_agent_state(
    root: &pi_session::PiSession,
    agent_id: &str,
    expected: &str,
) -> Value {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let snapshot = invoke(root, "list_agents", json!({})).await;
        if let Some(agent) = snapshot.details.as_ref().unwrap()["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|agent| agent["id"] == agent_id)
            && agent["state"] == expected
        {
            return agent.clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "agent {agent_id} did not reach {expected}: {:?}",
            snapshot.details
        );
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn desktop_commands_publish_final_state_without_reusing_tool_call_state() {
    let directory = tempfile::tempdir().unwrap();
    let (manager, root) = root(
        TestFactory::new([
            ScriptedTurn::WaitForAbort,
            ScriptedTurn::Text("follow-up result".into()),
        ]),
        &directory,
    )
    .await;
    let spawn = invoke(
        &root,
        "spawn_agent",
        json!({"agent":"reviewer","task":"review task"}),
    )
    .await;
    let id = spawn.details.as_ref().unwrap()["agentId"].as_str().unwrap();
    let started = desktop_widget(&root);
    assert_eq!(started["agents"][id]["state"], "running");
    assert_eq!(started["agents"][id]["task"], "review task");
    assert_eq!(
        started["agents"][id]["session"]["sessionId"],
        spawn.details.as_ref().unwrap()["sessionId"]
    );
    assert_eq!(started["liveAgentIds"], json!([id]));

    let stopped = root
        .current()
        .submit(pi_session::SessionInput::new(format!(
            "/subagents:interrupt {}",
            json!({"target":id}),
        )))
        .await
        .unwrap();
    assert!(matches!(stopped, pi_session::SubmitOutcome::Handled));
    invoke(
        &root,
        "wait_agent",
        json!({"targets":[id],"timeoutMs":2000}),
    )
    .await;
    assert_eq!(desktop_widget(&root)["agents"][id]["state"], "interrupted");

    root.current()
        .submit(pi_session::SessionInput::new(format!(
            "/subagents:followup {}",
            json!({"target":id,"task":"continue"}),
        )))
        .await
        .unwrap();
    invoke(
        &root,
        "wait_agent",
        json!({"targets":[id],"timeoutMs":2000}),
    )
    .await;
    let completed = desktop_widget(&root);
    assert_eq!(completed["agents"][id]["state"], "idle");
    assert_eq!(completed["liveAgentIds"], json!([id]));
    assert!(
        completed["agents"][id].get("lastReport").is_none(),
        "widgets never duplicate child transcripts"
    );
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn desktop_commands_reject_foreign_agents_and_reload_restores_live_ownership() {
    let directory = tempfile::tempdir().unwrap();
    let (manager, root) = root(TestFactory::new([ScriptedTurn::WaitForAbort]), &directory).await;
    let spawn = invoke(
        &root,
        "spawn_agent",
        json!({"agent":"reviewer","task":"wait"}),
    )
    .await;
    let id = spawn.details.as_ref().unwrap()["agentId"].as_str().unwrap();
    let other = manager
        .create_session(directory.path(), directory.path().join("other.jsonl"))
        .await
        .unwrap();
    let error = other
        .current()
        .submit(pi_session::SessionInput::new(format!(
            "/subagents:interrupt {}",
            json!({"target":id}),
        )))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not a direct child"));
    root.reload().await.unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let restored = invoke(&root, "list_agents", json!({})).await;
        if restored.details.as_ref().unwrap()["agents"][0]["state"] == "interrupted" {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "reloaded child ownership was not restored"
        );
        tokio::task::yield_now().await;
    }
    let restored = desktop_widget(&root);
    assert_eq!(restored["liveAgentIds"], json!([id]));
    root.current()
        .submit(pi_session::SessionInput::new(format!(
            "/subagents:followup {}",
            json!({"target":id,"task":"continue"}),
        )))
        .await
        .unwrap();
    invoke(&root, "interrupt_agent", json!({"target":id})).await;
    manager.shutdown().await.unwrap();
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
    let spawn_text = tool_text(&spawn);
    assert!(spawn_text.contains(&format!("Agent ID: {agent_id}")));
    assert!(spawn_text.contains(&format!("Child session ID: {spawned_session_id}")));

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
        first.details.as_ref().unwrap()["agents"][0]["lastReport"]["summary"],
        "first review"
    );
    assert_eq!(
        first.details.as_ref().unwrap()["agents"][0]["lastReport"]["outcome"],
        "succeeded"
    );
    let first_text = tool_text(&first);
    assert!(first_text.contains(&format!("Agent ID: {agent_id}")));
    assert!(first_text.contains(&format!("Child session ID: {spawned_session_id}")));
    assert!(first_text.contains("State: idle"));
    assert!(first_text.contains("Outcome: succeeded"));
    assert!(first_text.contains("Summary: first review"));
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
    let sent_text = tool_text(&sent);
    assert!(sent_text.contains(&format!("Target: {agent_id}")));
    assert!(sent_text.contains("Delivery: mailbox"));
    let follow = invoke(
        &root,
        "followup_task",
        json!({"target":agent_id,"task":"review again"}),
    )
    .await;
    assert_eq!(follow.details.as_ref().unwrap()["started"], true);
    let follow_text = tool_text(&follow);
    assert!(follow_text.contains(&format!("Agent ID: {agent_id}")));
    assert!(follow_text.contains("Follow-up started a new turn"));
    let second = invoke(
        &root,
        "wait_agent",
        json!({"targets":[agent_id],"mode":"all","timeoutMs":2000}),
    )
    .await;
    assert_eq!(
        second.details.as_ref().unwrap()["agents"][0]["lastReport"]["summary"],
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
    let list_text = tool_text(&list);
    assert!(list_text.contains(&format!("Agent ID: {agent_id}")));
    assert!(list_text.contains(&format!("Child session ID: {child_session_id}")));
    assert!(list_text.contains("State: idle"));
    assert!(list_text.contains("Summary: second review"));
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn child_message_sent_before_parent_wait_is_not_lost() {
    let directory = tempfile::tempdir().unwrap();
    let factory = TestFactory::new([ScriptedTurn::WaitForAbort])
        .with_root_turns([ScriptedTurn::WaitForAbort]);
    let providers = Arc::clone(&factory.providers);
    let (manager, root) = root(factory, &directory).await;

    let running_root = root.current();
    let root_turn = tokio::spawn(async move { running_root.prompt("hold parent open").await });
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while !root.current().runtime().agent().is_running()
        || !providers
            .lock()
            .unwrap()
            .iter()
            .any(|(depth, provider)| *depth == 0 && !provider.requests().is_empty())
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "parent turn did not start"
        );
        tokio::task::yield_now().await;
    }

    let spawn = invoke(
        &root,
        "spawn_agent",
        json!({"agent":"reviewer","task":"hold child open","detached":true}),
    )
    .await;
    let agent_id = spawn.details.as_ref().unwrap()["agentId"]
        .as_str()
        .unwrap()
        .to_string();
    let child_session_id = spawn.details.as_ref().unwrap()["sessionId"]
        .as_str()
        .unwrap();
    let child = manager
        .sessions()
        .into_iter()
        .find(|session| session.id() == child_session_id)
        .expect("spawned child session");

    let sent = invoke(
        &child,
        "send_message",
        json!({"target":"parent","message":"CHILD_READY"}),
    )
    .await;
    let event_id = sent.details.as_ref().unwrap()["eventRecordId"]
        .as_str()
        .unwrap()
        .to_string();
    let event_seq = sent.details.as_ref().unwrap()["eventRecordSeq"]
        .as_u64()
        .unwrap();

    // This deliberately begins after send_message has returned. The old
    // process-local waiter split routed the message away from wait_agent and
    // timed out here.
    let waited = invoke(
        &root,
        "wait_agent",
        json!({"targets":[agent_id],"mode":"any","timeoutMs":200}),
    )
    .await;
    let details = waited.details.as_ref().unwrap();
    assert!(format!("{:?}", waited.content).contains("CHILD_READY"));
    assert_eq!(details["state"], "ready");
    assert_eq!(details["messages"][0]["id"], event_id);
    assert_eq!(details["messages"][0]["sequence"], event_seq);
    assert_eq!(details["messages"][0]["from"], agent_id);
    assert_eq!(details["messages"][0]["message"], "CHILD_READY");
    let repeated = invoke(
        &root,
        "wait_agent",
        json!({"targets":[agent_id],"mode":"any","timeoutMs":0}),
    )
    .await;
    assert_eq!(repeated.details.as_ref().unwrap()["timedOut"], true);

    let record = root
        .current()
        .log()
        .load()
        .unwrap()
        .entries
        .into_iter()
        .find(|record| record.id == event_id)
        .expect("durable collaboration event");
    let record = serde_json::to_value(record).unwrap();
    assert_eq!(record["seq"], event_seq);
    assert_eq!(record["customType"], "pi.subagents.event");
    assert_eq!(record["data"]["kind"], "message");
    assert_eq!(record["data"]["recipientSessionId"], root.id());

    invoke(&root, "interrupt_agent", json!({"target":agent_id})).await;
    root.current().abort();
    root_turn.await.unwrap().unwrap();
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn child_message_to_an_idle_parent_is_durable_before_projection() {
    let directory = tempfile::tempdir().unwrap();
    let factory = TestFactory::new([ScriptedTurn::WaitForAbort])
        .with_root_turns([ScriptedTurn::WaitForAbort]);
    let (manager, root) = root(factory, &directory).await;
    let spawn = invoke(
        &root,
        "spawn_agent",
        json!({"agent":"reviewer","task":"hold child open","detached":true}),
    )
    .await;
    let agent_id = spawn.details.as_ref().unwrap()["agentId"]
        .as_str()
        .unwrap()
        .to_string();
    let child_session_id = spawn.details.as_ref().unwrap()["sessionId"]
        .as_str()
        .unwrap();
    let child = manager
        .sessions()
        .into_iter()
        .find(|session| session.id() == child_session_id)
        .expect("spawned child session");

    assert!(!root.current().runtime().agent().is_running());
    let sent = invoke(
        &child,
        "send_message",
        json!({"target":"parent","message":"WAKE_IDLE_PARENT"}),
    )
    .await;
    let event_id = sent.details.as_ref().unwrap()["eventRecordId"]
        .as_str()
        .unwrap();
    assert!(
        root.current()
            .log()
            .load()
            .unwrap()
            .entries
            .iter()
            .any(|record| record.id == event_id)
    );

    invoke(&root, "interrupt_agent", json!({"target":agent_id})).await;
    root.current().abort();
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn parent_message_sent_before_child_wait_is_not_lost() {
    let directory = tempfile::tempdir().unwrap();
    let (manager, root) = root(TestFactory::new([ScriptedTurn::WaitForAbort]), &directory).await;
    let spawn = invoke(
        &root,
        "spawn_agent",
        json!({"agent":"reviewer","task":"hold child open","detached":true}),
    )
    .await;
    let agent_id = spawn.details.as_ref().unwrap()["agentId"]
        .as_str()
        .unwrap()
        .to_string();
    let child_session_id = spawn.details.as_ref().unwrap()["sessionId"]
        .as_str()
        .unwrap();
    let child = manager
        .sessions()
        .into_iter()
        .find(|session| session.id() == child_session_id)
        .expect("spawned child session");

    let sent = invoke(
        &root,
        "send_message",
        json!({"target":agent_id,"message":"PARENT_READY"}),
    )
    .await;
    let event_id = sent.details.as_ref().unwrap()["eventRecordId"]
        .as_str()
        .unwrap();

    let waited = invoke(
        &child,
        "wait_agent",
        json!({"targets":["parent"],"mode":"any","timeoutMs":200}),
    )
    .await;
    let details = waited.details.as_ref().unwrap();
    assert!(format!("{:?}", waited.content).contains("PARENT_READY"));
    assert_eq!(details["state"], "ready");
    assert_eq!(details["messages"][0]["id"], event_id);
    assert_eq!(details["messages"][0]["from"], "parent");
    assert_eq!(details["messages"][0]["message"], "PARENT_READY");

    invoke(&root, "interrupt_agent", json!({"target":agent_id})).await;
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
    let cancellation = invoke(&root, "interrupt_agent", json!({"target":agent_id})).await;
    assert!(tool_text(&cancellation).contains(&format!("Agent ID: {agent_id}")));
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
    assert!(tool_text(&interrupted).contains("State: interrupted"));

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
        recovered.details.as_ref().unwrap()["agents"][0]["lastReport"]["summary"],
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

#[tokio::test]
async fn completed_reports_are_bounded_before_entering_the_parent_context() {
    let directory = tempfile::tempdir().unwrap();
    let result = "result-".repeat(4_000);
    let (manager, root) = root(
        TestFactory::new([ScriptedTurn::Text(result.clone())]),
        &directory,
    )
    .await;
    let spawn = invoke(
        &root,
        "spawn_agent",
        json!({"agent":"reviewer","task":"return a large report"}),
    )
    .await;
    let agent_id = spawn.details.as_ref().unwrap()["agentId"]
        .as_str()
        .unwrap()
        .to_string();

    let completed = invoke(
        &root,
        "wait_agent",
        json!({"targets":[agent_id],"mode":"all","timeoutMs":2000}),
    )
    .await;
    let report = &completed.details.as_ref().unwrap()["agents"][0]["lastReport"];
    let summary = report["summary"].as_str().unwrap();
    assert_eq!(report["truncated"], true);
    assert!(summary.ends_with('…'));
    assert!(summary.len() < result.len());
    assert!(summary.len() <= 16 * 1024);
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn profile_without_model_or_thinking_inherits_the_parent_selection() {
    let directory = tempfile::tempdir().unwrap();
    let agent_root = directory.path().join("agents");
    std::fs::create_dir_all(&agent_root).unwrap();
    std::fs::write(
        agent_root.join("inherited.md"),
        "---\nname: inherited\ndescription: Profile with no runtime selection\ntools: read\n---\nComplete the task.",
    )
    .unwrap();
    let mut factory = TestFactory::new([ScriptedTurn::Text("done".into())]);
    factory.agent_paths.push(agent_root);
    let (manager, root) = root(factory, &directory).await;
    let spawn = invoke(
        &root,
        "spawn_agent",
        json!({
            "agent":"inherited",
            "task":"inherit the current runtime selection",
            "detached":true
        }),
    )
    .await;
    let child_session_id = spawn.details.as_ref().unwrap()["sessionId"]
        .as_str()
        .unwrap();
    let child = manager
        .sessions()
        .into_iter()
        .find(|session| session.id() == child_session_id)
        .expect("spawned child session");
    let child_session = child.current();
    let agent = child_session.runtime().agent();
    assert_eq!(
        agent.model_selection(),
        (ProviderId::new("scripted"), ModelId::new("test"))
    );
    assert_eq!(agent.thinking_level(), pi_core::ThinkingLevel::Off);
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn spawn_fork_turns_keeps_only_the_requested_recent_parent_turns() {
    let directory = tempfile::tempdir().unwrap();
    let factory = TestFactory::new([ScriptedTurn::Text("child result".into())]).with_root_turns([
        ScriptedTurn::Text("answer one".into()),
        ScriptedTurn::Text("answer two".into()),
        ScriptedTurn::Text("answer three".into()),
    ]);
    let providers = Arc::clone(&factory.providers);
    let (manager, root) = root(factory, &directory).await;
    root.current().prompt("user one").await.unwrap();
    root.current().prompt("user two").await.unwrap();
    root.current().prompt("user three").await.unwrap();

    let spawn = invoke(
        &root,
        "spawn_agent",
        json!({
            "agent":"reviewer",
            "task":"use recent context",
            "fork_turns":"2",
            "detached":true
        }),
    )
    .await;
    let id = spawn.details.as_ref().unwrap()["agentId"].as_str().unwrap();
    wait_for_agent_state(&root, id, "idle").await;

    let child_request = providers
        .lock()
        .unwrap()
        .iter()
        .filter(|(depth, _)| *depth == 1)
        .find_map(|(_, provider)| provider.requests().into_iter().next())
        .expect("child provider request");
    let messages = serde_json::to_string(&child_request.messages).unwrap();
    assert!(!messages.contains("user one"));
    assert!(!messages.contains("answer one"));
    assert!(messages.contains("user two"));
    assert!(messages.contains("answer two"));
    assert!(messages.contains("user three"));
    assert!(messages.contains("answer three"));
    assert!(messages.contains("use recent context"));
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn completed_reports_join_the_parent_context_unless_detached() {
    let directory = tempfile::tempdir().unwrap();
    let (manager, root) = root(
        TestFactory::new([
            ScriptedTurn::Text("joined result".into()),
            ScriptedTurn::Text("detached result".into()),
        ]),
        &directory,
    )
    .await;
    let joined = invoke(
        &root,
        "spawn_agent",
        json!({"agent":"reviewer","task":"join this result"}),
    )
    .await;
    let joined_id = joined.details.as_ref().unwrap()["agentId"]
        .as_str()
        .unwrap();

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let reports = joined_reports(&root);
        if let Some(report) = reports.first() {
            assert_eq!(report["details"]["agentId"], joined_id);
            assert_eq!(report["details"]["report"]["summary"], "joined result");
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "completed report did not join the parent context: agents={:?}, entries={:?}",
            invoke(&root, "list_agents", json!({})).await.details,
            root.current().log().load().unwrap().entries
        );
        tokio::task::yield_now().await;
    }

    let detached = invoke(
        &root,
        "spawn_agent",
        json!({"agent":"reviewer","task":"do not join this result","detached":true}),
    )
    .await;
    let detached_id = detached.details.as_ref().unwrap()["agentId"]
        .as_str()
        .unwrap()
        .to_string();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let snapshot = invoke(&root, "list_agents", json!({})).await;
        let detached = snapshot.details.as_ref().unwrap()["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|agent| agent["id"] == detached_id)
            .unwrap();
        if detached["state"] == "idle" {
            assert_eq!(detached["detached"], true);
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "detached agent did not settle"
        );
        tokio::task::yield_now().await;
    }
    tokio::task::yield_now().await;
    assert_eq!(joined_reports(&root).len(), 1);
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn launches_over_the_root_concurrency_limit_queue_and_start_fifo() {
    let directory = tempfile::tempdir().unwrap();
    let (manager, root) = root(TestFactory::new([ScriptedTurn::WaitForAbort]), &directory).await;
    let mut ids = Vec::new();
    for index in 0..10 {
        let spawn = invoke(
            &root,
            "spawn_agent",
            json!({"agent":"reviewer","task":format!("task {index}")}),
        )
        .await;
        ids.push(
            spawn.details.as_ref().unwrap()["agentId"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    let snapshot = invoke(&root, "list_agents", json!({})).await;
    assert_eq!(
        snapshot.details.as_ref().unwrap()["agents"][8]["state"],
        "queued"
    );
    assert_eq!(
        snapshot.details.as_ref().unwrap()["agents"][9]["state"],
        "queued"
    );

    invoke(&root, "interrupt_agent", json!({"target":ids[0]})).await;
    invoke(
        &root,
        "wait_agent",
        json!({"targets":[ids[0]],"mode":"all","timeoutMs":2000}),
    )
    .await;

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let snapshot = invoke(&root, "list_agents", json!({})).await;
        if snapshot.details.as_ref().unwrap()["agents"][8]["state"] == "running"
            && snapshot.details.as_ref().unwrap()["agents"][9]["state"] == "queued"
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "queued agent did not start"
        );
        tokio::task::yield_now().await;
    }

    invoke(&root, "interrupt_agent", json!({"target":ids[1]})).await;
    invoke(
        &root,
        "wait_agent",
        json!({"targets":[ids[1]],"mode":"all","timeoutMs":2000}),
    )
    .await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let snapshot = invoke(&root, "list_agents", json!({})).await;
        if snapshot.details.as_ref().unwrap()["agents"][9]["state"] == "running" {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "second queued agent did not start in FIFO order"
        );
        tokio::task::yield_now().await;
    }

    for id in &ids[2..] {
        invoke(&root, "interrupt_agent", json!({"target":id})).await;
    }
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn process_restart_reattaches_completed_agent_for_a_new_turn() {
    let directory = tempfile::tempdir().unwrap();
    let parent_path = directory.path().join("parent.jsonl");
    let (manager, root) = root(
        TestFactory::new([ScriptedTurn::Text("before restart".into())]),
        &directory,
    )
    .await;
    root.current().log().materialize().unwrap();
    let spawn = invoke(
        &root,
        "spawn_agent",
        json!({"agent":"reviewer","task":"finish once","detached":true}),
    )
    .await;
    let agent_id = spawn.details.as_ref().unwrap()["agentId"]
        .as_str()
        .unwrap()
        .to_string();
    let child_session_id = spawn.details.as_ref().unwrap()["sessionId"]
        .as_str()
        .unwrap()
        .to_string();
    let first = invoke(
        &root,
        "wait_agent",
        json!({"targets":[agent_id],"mode":"all","timeoutMs":2000}),
    )
    .await;
    assert_eq!(
        first.details.as_ref().unwrap()["agents"][0]["lastReport"]["summary"],
        "before restart"
    );
    manager.shutdown().await.unwrap();
    drop(root);

    let factory = TestFactory::new([ScriptedTurn::Text("after restart".into())]);
    let providers = factory.providers.clone();
    let manager = MultiSessionManager::new(factory);
    let root = manager.open_session(&parent_path).await.unwrap();
    let restored = wait_for_agent_state(&root, &agent_id, "idle").await;
    assert_eq!(restored["childSessionId"], child_session_id);
    assert_eq!(restored["lastReport"]["summary"], "before restart");
    assert_eq!(
        providers
            .lock()
            .unwrap()
            .iter()
            .filter(|(depth, _)| *depth > 0)
            .map(|(_, provider)| provider.requests().len())
            .sum::<usize>(),
        0,
        "reattaching an idle child must not start a provider turn"
    );

    invoke(
        &root,
        "followup_task",
        json!({"target":agent_id,"task":"continue safely"}),
    )
    .await;
    let completed = wait_for_agent_state(&root, &agent_id, "idle").await;
    assert_eq!(completed["childSessionId"], child_session_id);
    assert_eq!(completed["lastReport"]["summary"], "after restart");
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn process_restart_interrupts_running_turn_but_replays_queued_launches_only() {
    let directory = tempfile::tempdir().unwrap();
    let parent_path = directory.path().join("parent.jsonl");
    let (manager, root) = root(TestFactory::new([ScriptedTurn::WaitForAbort]), &directory).await;
    root.current().log().materialize().unwrap();
    let mut ids = Vec::new();
    for index in 0..9 {
        let spawn = invoke(
            &root,
            "spawn_agent",
            json!({"agent":"reviewer","task":format!("restart task {index}"),"detached":true}),
        )
        .await;
        ids.push(
            spawn.details.as_ref().unwrap()["agentId"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    let before = invoke(&root, "list_agents", json!({})).await;
    assert_eq!(
        before.details.as_ref().unwrap()["agents"][0]["state"],
        "running"
    );
    assert_eq!(
        before.details.as_ref().unwrap()["agents"][8]["state"],
        "queued"
    );

    // Drop without lifecycle shutdown to leave the last durable running/queued
    // checkpoints exactly as a terminated process would.
    drop(root);
    drop(manager);
    tokio::task::yield_now().await;

    let factory = TestFactory::new([ScriptedTurn::Text("recovered queued task".into())]);
    let providers = factory.providers.clone();
    let manager = MultiSessionManager::new(factory);
    let root = manager.open_session(&parent_path).await.unwrap();
    let interrupted = wait_for_agent_state(&root, &ids[0], "interrupted").await;
    assert_eq!(interrupted["lastReport"]["outcome"], "interrupted");
    assert!(
        interrupted["lastReport"]["summary"]
            .as_str()
            .unwrap()
            .contains("No tool call was replayed")
    );
    let queued = wait_for_agent_state(&root, &ids[8], "idle").await;
    assert_eq!(queued["lastReport"]["summary"], "recovered queued task");
    assert_eq!(
        providers
            .lock()
            .unwrap()
            .iter()
            .filter(|(depth, _)| *depth > 0)
            .map(|(_, provider)| provider.requests().len())
            .sum::<usize>(),
        1,
        "only the never-started queued launch may execute after restart"
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
