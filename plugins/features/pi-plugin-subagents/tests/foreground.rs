use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_agent::AgentOptions;
use pi_core::{
    Message, ModelId, PluginContext, PresentationMode, ProviderId, TextContent, ToolCall,
    ToolCallId, UserMessage,
};
use pi_plugin_find::FindPlugin;
use pi_plugin_grep::GrepPlugin;
use pi_plugin_ls::LsPlugin;
use pi_plugin_read::ReadPlugin;
use pi_plugin_skills::{SkillLoaderOptions, SkillsPlugin};
use pi_plugin_subagents::{
    SubagentLoaderOptions, SubagentRuntime, SubagentSkillPromptProjector, SubagentsPlugin,
    SubagentsSessionPlugin,
};
use pi_runtime::PiRuntime;
use pi_session::{
    AgentSession, AgentSessionOptions, AgentSessionRuntimeFactory, AgentSessionRuntimeRequest,
    AgentSessionRuntimeTarget, MultiSessionManager, PiPluginContext, PluginContextBinding,
    PreparedAgentSession, SessionError, SessionPlugins,
};
use pi_test_support::{ScriptedProvider, ScriptedProviderPlugin, ScriptedTurn};
use serde_json::json;

type RecordedProviders = Arc<Mutex<Vec<(usize, Arc<ScriptedProvider>)>>>;

#[derive(Clone)]
struct TestFactory {
    subagents: SubagentRuntime,
    binding: PluginContextBinding,
    providers: RecordedProviders,
    nested: bool,
    agent_paths: Vec<PathBuf>,
    root_agent: String,
    root_context: Option<pi_core::IsolatedContextMode>,
    batch_size: usize,
    supervision: bool,
    blocked_sibling: bool,
}

impl TestFactory {
    fn new() -> Self {
        Self {
            subagents: SubagentRuntime::default(),
            binding: PluginContextBinding::new(),
            providers: Arc::new(Mutex::new(Vec::new())),
            nested: false,
            agent_paths: Vec::new(),
            root_agent: "reviewer".to_string(),
            root_context: None,
            batch_size: 1,
            supervision: false,
            blocked_sibling: false,
        }
    }

    fn nested() -> Self {
        Self {
            nested: true,
            agent_paths: vec![
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/agents"),
            ],
            ..Self::new()
        }
    }

    fn with_root_agent(mut self, root_agent: &str) -> Self {
        self.root_agent = root_agent.to_string();
        self
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
        let turns = if self.supervision && depth > 0 {
            let existing_children = self
                .providers
                .lock()
                .unwrap()
                .iter()
                .filter(|(depth, _)| *depth > 0)
                .count();
            if self.blocked_sibling && existing_children > 0 {
                vec![ScriptedTurn::WaitForAbort]
            } else {
                vec![
                    ScriptedTurn::ToolCalls(vec![ToolCall::new(
                        "ask-parent",
                        "contact_supervisor",
                        json!({"reason":"interview_request","message":"Choose compatibility policy", "interview":{"title":"Compatibility","questions":[]}}),
                    )]),
                    ScriptedTurn::Text("child continued with supervisor decision".into()),
                ]
            }
        } else if self.nested {
            nested_turns(depth)
        } else if depth > 0 {
            vec![ScriptedTurn::Text("child review complete".to_string())]
        } else {
            let mut turns = vec![
                ScriptedTurn::ToolCalls((0..self.batch_size).map(|index| ToolCall::new(
                    ToolCallId::new(format!("delegate-{index}")),
                    "subagent",
                    {
                        let mut args = json!({"agent": self.root_agent, "task": "Review the parser"});
                        if let Some(mode) = self.root_context {
                            args["context"] = json!(mode);
                        }
                        args
                    },
                )).collect()),
                ScriptedTurn::Text("parent incorporated the review".to_string()),
            ];
            if self.supervision && !self.blocked_sibling {
                turns.extend([
                    ScriptedTurn::ToolCalls(vec![ToolCall::new(
                        "answer-child",
                        "subagent_supervisor",
                        json!({"action":"reply","message":"{\"compatible\":true}"}),
                    )]),
                    ScriptedTurn::ToolCalls(vec![ToolCall::new(
                        "wait-child",
                        "bg_wait",
                        json!({"timeoutMs":1000}),
                    )]),
                    ScriptedTurn::Text("supervision complete".into()),
                ]);
            }
            if restored_log.is_some() {
                turns.drain(..2);
            }
            turns
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
        let skill_options = SkillLoaderOptions::new(&cwd, &cwd);
        let skill_projector = Arc::new(SubagentSkillPromptProjector::new(self.subagents.clone()));
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
                            "subagent",
                            "contact_supervisor",
                            "subagent_supervisor",
                            "bg_wait",
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
            .agent_plugin(SkillsPlugin::load_with_prompt_projector(
                skill_options,
                skill_projector,
            ))
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

fn nested_turns(depth: usize) -> Vec<ScriptedTurn> {
    match depth {
        0 => vec![
            ScriptedTurn::ToolCalls(vec![ToolCall::new(
                ToolCallId::new("root-delegate"),
                "subagent",
                json!({"agent": "smoke-delegate", "task": "Continue through child depth 6"}),
            )]),
            ScriptedTurn::Text("root incorporated six-level work".to_string()),
        ],
        1..=5 => vec![
            ScriptedTurn::ToolCalls(vec![ToolCall::new(
                ToolCallId::new(format!("delegate-depth-{depth}")),
                "subagent",
                json!({"agent": "smoke-delegate", "task": "Continue through child depth 6"}),
            )]),
            ScriptedTurn::Text(format!("delegate depth {depth} incorporated its child")),
        ],
        6 => vec![ScriptedTurn::Text(
            "delegate depth 6 completed the leaf inspection".to_string(),
        )],
        _ => panic!("unexpected isolated depth {depth}"),
    }
}

#[tokio::test]
async fn foreground_tool_runs_a_profiled_child_through_the_shared_session_manager() {
    let directory = tempfile::tempdir().unwrap();
    let factory = TestFactory::new();
    let providers = Arc::clone(&factory.providers);
    let manager = MultiSessionManager::new(factory);
    let root_path = directory.path().join("primary.jsonl");
    let root = manager
        .create_session(directory.path(), &root_path)
        .await
        .unwrap();

    let outcome = root
        .current()
        .prompt(vec![Message::User(UserMessage {
            content: vec![pi_core::ContentBlock::Text(TextContent::new(
                "Delegate a review",
            ))],
            timestamp_ms: 0,
        })])
        .await
        .unwrap();

    let subagent_result = outcome
        .new_messages
        .iter()
        .find_map(|message| match message {
            Message::ToolResult(result) if result.tool_name == "subagent" => Some(result),
            _ => None,
        })
        .expect("parent should persist the subagent tool result");
    assert!(!subagent_result.is_error);
    assert!(subagent_result.content.iter().any(|content| {
        matches!(content, pi_core::ContentBlock::Text(text) if text.text == "child review complete")
    }));
    assert_eq!(root.path(), PathBuf::from(&root_path));
    assert_eq!(manager.sessions().len(), 2);

    let child_provider = providers
        .lock()
        .unwrap()
        .iter()
        .find_map(|(depth, provider)| (*depth == 1).then(|| Arc::clone(provider)))
        .expect("child provider should be recorded");
    let requests = child_provider.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0]
            .system_prompt
            .starts_with("You are a child subagent, not the parent orchestrator.")
    );
    assert!(
        requests[0]
            .system_prompt
            .contains("<active_agent name=\"reviewer\"/>")
    );
    assert!(
        requests[0]
            .system_prompt
            .contains("You are a disciplined review subagent.")
    );
    assert!(
        !requests[0]
            .system_prompt
            .contains("Delegated subagent role")
    );
    assert!(!requests[0].tools.iter().any(|tool| tool.name == "subagent"));

    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn parallel_fork_children_inherit_the_request_but_not_the_launch_batch() {
    let directory = tempfile::tempdir().unwrap();
    let mut factory = TestFactory::new();
    factory.root_context = Some(pi_core::IsolatedContextMode::Fork);
    factory.batch_size = 2;
    let providers = Arc::clone(&factory.providers);
    let manager = MultiSessionManager::new(factory);
    let root = manager
        .create_session(directory.path(), directory.path().join("primary.jsonl"))
        .await
        .unwrap();
    let outcome = root
        .current()
        .prompt("inherited request for both children")
        .await
        .unwrap();
    assert_eq!(
        outcome
            .new_messages
            .iter()
            .filter(|message| matches!(message, Message::ToolResult(result) if !result.is_error))
            .count(),
        2
    );
    let children = providers
        .lock()
        .unwrap()
        .iter()
        .filter(|(depth, _)| *depth == 1)
        .map(|(_, provider)| provider.requests())
        .collect::<Vec<_>>();
    assert_eq!(children.len(), 2);
    for requests in &children {
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].messages.len(), 2);
        assert!(
            matches!(&requests[0].messages[0], Message::User(user) if user.content.iter().any(|block| matches!(block, pi_core::ContentBlock::Text(text) if text.text == "inherited request for both children")))
        );
        assert!(
            requests[0]
                .messages
                .iter()
                .all(|message| matches!(message, Message::User(_)))
        );
    }
    assert_eq!(children[0][0].messages[0], children[1][0].messages[0]);
    manager.shutdown().await.unwrap();
}

fn user_message(text: &str) -> Message {
    Message::User(UserMessage {
        content: vec![pi_core::ContentBlock::Text(TextContent::new(text))],
        timestamp_ms: 0,
    })
}

#[tokio::test]
async fn supervisor_reply_continues_the_same_isolated_session() {
    supervisor_roundtrip(false).await;
}

#[tokio::test]
async fn supervisor_request_survives_parent_generation_reload() {
    supervisor_roundtrip(true).await;
}

#[tokio::test]
async fn replacement_after_reload_aborts_detached_children_before_retiring_control() {
    let directory = tempfile::tempdir().unwrap();
    let mut factory = TestFactory::new();
    factory.supervision = true;
    factory.blocked_sibling = true;
    factory.batch_size = 2;
    let manager = MultiSessionManager::new(factory);
    let root = manager
        .create_session(directory.path(), directory.path().join("root.jsonl"))
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        root.current().prompt(vec![user_message("Delegate")]),
    )
    .await
    .unwrap()
    .unwrap();
    let children = manager
        .sessions()
        .into_iter()
        .filter(|session| session.id() != root.id())
        .collect::<Vec<_>>();
    assert_eq!(children.len(), 2);
    root.reload().await.unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        root.new_session(directory.path(), directory.path().join("replacement.jsonl")),
    )
    .await
    .expect("replacement must drain children with the current control handle")
    .unwrap();
    for child in children {
        assert!(!child.current().runtime().agent().is_running());
    }
    tokio::time::timeout(std::time::Duration::from_secs(5), manager.shutdown())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn dropping_manager_with_real_detached_waiters_releases_children() {
    let directory = tempfile::tempdir().unwrap();
    let mut factory = TestFactory::new();
    factory.supervision = true;
    factory.blocked_sibling = true;
    factory.batch_size = 2;
    let manager = MultiSessionManager::new(factory);
    let root = manager
        .create_session(directory.path(), directory.path().join("root.jsonl"))
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        root.current().prompt(vec![user_message("Delegate")]),
    )
    .await
    .unwrap()
    .unwrap();
    let weak_children = manager
        .sessions()
        .into_iter()
        .filter(|session| session.id() != root.id())
        .map(|session| Arc::downgrade(&session.current()))
        .collect::<Vec<_>>();
    assert_eq!(weak_children.len(), 2);
    drop(manager);
    drop(root);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while weak_children.iter().any(|child| child.upgrade().is_some()) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("real adapter waiters must not retain their task owners after manager drop");
}

#[tokio::test]
async fn shutdown_drains_detached_children_waiting_for_supervisor_reply() {
    let directory = tempfile::tempdir().unwrap();
    let mut factory = TestFactory::new();
    factory.supervision = true;
    let manager = MultiSessionManager::new(factory);
    let root = manager
        .create_session(directory.path(), directory.path().join("root.jsonl"))
        .await
        .unwrap();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        root.current().prompt(vec![user_message("Delegate")]),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(outcome.new_messages.iter().any(|message| matches!(message,
        Message::ToolResult(result) if result.tool_name == "subagent" && result.details.as_ref().is_some_and(|details| details["detached"] == true)
    )));
    let children = manager
        .sessions()
        .into_iter()
        .filter(|session| session.id() != root.id())
        .collect::<Vec<_>>();
    assert_eq!(children.len(), 1);
    tokio::time::timeout(std::time::Duration::from_secs(5), manager.shutdown())
        .await
        .expect("shutdown must not require a supervisor reply")
        .unwrap();
    assert!(manager.sessions().is_empty());
    for child in children {
        assert!(child.current().is_closed());
        assert!(!child.current().runtime().agent().is_running());
    }
}

async fn supervisor_roundtrip(reload: bool) {
    let directory = tempfile::tempdir().unwrap();
    let mut factory = TestFactory::new();
    factory.supervision = true;
    let providers = factory.providers.clone();
    let manager = MultiSessionManager::new(factory);
    let root = manager
        .create_session(directory.path(), directory.path().join("root.jsonl"))
        .await
        .unwrap();
    let first = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        root.current().prompt(vec![user_message("Delegate")]),
    )
    .await
    .expect("supervisor request must release foreground wait")
    .unwrap();
    let detached = first
        .new_messages
        .iter()
        .find_map(|message| match message {
            Message::ToolResult(result) if result.tool_name == "subagent" => result.details.clone(),
            _ => None,
        })
        .unwrap();
    assert_eq!(detached["detached"], true);
    assert_eq!(detached["activityState"], "needs_attention");
    assert_eq!(manager.sessions().len(), 2);
    let child = providers
        .lock()
        .unwrap()
        .iter()
        .find(|(depth, _)| *depth == 1)
        .unwrap()
        .1
        .clone();
    assert_eq!(
        child.requests().len(),
        1,
        "child is still waiting inside contact_supervisor"
    );
    if reload {
        root.reload().await.unwrap();
    }
    let next = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        root.current()
            .prompt(vec![user_message("Keep compatibility")]),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        manager.sessions().len(),
        2,
        "reply must not launch a replacement session"
    );
    let requests = child.requests();
    assert_eq!(requests.len(), 2);
    let reply = requests[1]
        .messages
        .iter()
        .find_map(|message| match message {
            Message::ToolResult(result) if result.tool_name == "contact_supervisor" => Some(result),
            _ => None,
        })
        .unwrap();
    assert!(!reply.is_error);
    assert_eq!(
        reply.details.as_ref().unwrap()["structuredReply"]["compatible"],
        true
    );
    assert!(next.new_messages.iter().any(|message| matches!(message, Message::ToolResult(result) if result.tool_name == "subagent_supervisor" && !result.is_error)));
    let parent = providers
        .lock()
        .unwrap()
        .iter()
        .find(|(depth, _)| *depth == 0)
        .unwrap()
        .1
        .clone();
    for request in parent.requests() {
        assert_tool_pairs(&request.messages);
    }
    assert!(
        std::fs::read_to_string(root.path())
            .unwrap()
            .contains("subagent_supervisor_reply")
    );
    manager.shutdown().await.unwrap();
}

fn assert_tool_pairs(messages: &[Message]) {
    let mut pending = std::collections::HashSet::new();
    for message in messages {
        match message {
            Message::Assistant(message) => {
                for call in message.tool_calls() {
                    assert!(pending.insert(call.id.clone()));
                }
            }
            Message::ToolResult(result) => {
                assert!(
                    pending.remove(&result.tool_call_id),
                    "result must have its call"
                );
            }
            _ => {}
        }
    }
    assert!(
        pending.is_empty(),
        "provider request must contain every tool result"
    );
}

#[tokio::test]
async fn one_supervisor_request_releases_parallel_sibling_waits() {
    let directory = tempfile::tempdir().unwrap();
    let mut factory = TestFactory::new();
    factory.supervision = true;
    factory.blocked_sibling = true;
    factory.batch_size = 2;
    let providers = factory.providers.clone();
    let manager = MultiSessionManager::new(factory);
    let root = manager
        .create_session(directory.path(), directory.path().join("root.jsonl"))
        .await
        .unwrap();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        root.current()
            .prompt(vec![user_message("Delegate in parallel")]),
    )
    .await
    .expect("a running sibling must not hold the parent hostage")
    .unwrap();
    let results = outcome
        .new_messages
        .iter()
        .filter_map(|message| match message {
            Message::ToolResult(result) if result.tool_name == "subagent" => Some(result),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 2);
    assert!(
        results
            .iter()
            .all(|result| result.details.as_ref().unwrap()["detached"] == true)
    );
    let parent = providers
        .lock()
        .unwrap()
        .iter()
        .find(|(depth, _)| *depth == 0)
        .unwrap()
        .1
        .clone();
    assert_tool_pairs(&parent.requests()[1].messages);
    tokio::time::timeout(std::time::Duration::from_secs(5), manager.shutdown())
        .await
        .expect("closing parent must cancel waiting descendants")
        .unwrap();
}

#[tokio::test]
async fn explicit_fresh_overrides_a_markdown_fork_default() {
    let directory = tempfile::tempdir().unwrap();
    let agents = directory.path().join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(agents.join("forker.md"), "---\nname: forker\ndescription: fork by default\ndefaultContext: fork\ntools: read\n---\nInspect the task.\n").unwrap();
    for context in [None, Some(pi_core::IsolatedContextMode::Fresh)] {
        let mut factory = TestFactory::new().with_root_agent("forker");
        factory.root_context = context;
        let providers = Arc::clone(&factory.providers);
        let manager = MultiSessionManager::new(factory);
        let root = manager
            .create_session(
                directory.path(),
                directory.path().join(format!("{context:?}.jsonl")),
            )
            .await
            .unwrap();
        let outcome = root.current().prompt("parent-only request").await.unwrap();
        assert!(
            outcome
                .new_messages
                .iter()
                .any(|message| matches!(message, Message::ToolResult(result) if !result.is_error))
        );
        let requests = providers
            .lock()
            .unwrap()
            .iter()
            .find(|(depth, _)| *depth == 1)
            .unwrap()
            .1
            .requests();
        assert_eq!(
            requests[0].messages.len(),
            if context.is_none() { 2 } else { 1 }
        );
        manager.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn feature_config_zero_depth_blocks_before_the_session_manager_creates_a_child() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("extensions/subagent/config.json");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, r#"{"maxSubagentDepth": 0}"#).unwrap();
    let manager = MultiSessionManager::new(TestFactory::new());
    let root = manager
        .create_session(directory.path(), directory.path().join("primary.jsonl"))
        .await
        .unwrap();

    let outcome = root
        .current()
        .prompt(vec![Message::User(UserMessage {
            content: vec![pi_core::ContentBlock::Text(TextContent::new(
                "Attempt a blocked delegation",
            ))],
            timestamp_ms: 0,
        })])
        .await
        .unwrap();

    let result = outcome
        .new_messages
        .iter()
        .find_map(|message| match message {
            Message::ToolResult(result) if result.tool_name == "subagent" => Some(result),
            _ => None,
        })
        .expect("the blocked tool call should be persisted");
    assert!(result.is_error);
    assert!(result.content.iter().any(|content| {
        matches!(content, pi_core::ContentBlock::Text(text) if text.text.contains("nesting limit reached"))
    }));
    assert_eq!(manager.sessions().len(), 1);

    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn markdown_defined_delegate_recurses_through_six_child_depths() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("extensions/subagent/config.json");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, r#"{"maxSubagentDepth": 6}"#).unwrap();
    let factory = TestFactory::nested();
    let providers = Arc::clone(&factory.providers);
    let manager = MultiSessionManager::new(factory);
    let root = manager
        .create_session(directory.path(), directory.path().join("primary.jsonl"))
        .await
        .unwrap();

    let outcome = root
        .current()
        .prompt(vec![Message::User(UserMessage {
            content: vec![pi_core::ContentBlock::Text(TextContent::new(
                "Delegate recursively",
            ))],
            timestamp_ms: 0,
        })])
        .await
        .unwrap();

    let root_tool_result = outcome
        .new_messages
        .iter()
        .find_map(|message| match message {
            Message::ToolResult(result) if result.tool_name == "subagent" => Some(result),
            _ => None,
        })
        .expect("root should receive its child result");
    assert!(root_tool_result.content.iter().any(|content| {
        matches!(content, pi_core::ContentBlock::Text(text) if text.text == "delegate depth 1 incorporated its child")
    }));
    assert_eq!(manager.sessions().len(), 7);

    {
        let providers = providers.lock().unwrap();
        let mut depths = providers
            .iter()
            .map(|(depth, _)| *depth)
            .collect::<Vec<_>>();
        depths.sort_unstable();
        assert_eq!(depths, (0..=6).collect::<Vec<_>>());
        for depth in 1..=6 {
            let requests = providers
                .iter()
                .find(|(candidate, _)| *candidate == depth)
                .unwrap_or_else(|| panic!("provider for child depth {depth} should exist"))
                .1
                .requests();
            assert!(requests[0].system_prompt.starts_with(
                "You are a child subagent with explicit fanout responsibility for this assigned task."
            ));
            assert!(
                requests[0]
                    .system_prompt
                    .contains("<active_agent name=\"smoke-delegate\"/>")
            );
            assert!(
                requests[0]
                    .system_prompt
                    .contains("You are one level in the six-level recursive pi-rs subagent test.")
            );
            assert!(
                !requests[0]
                    .system_prompt
                    .contains("Delegated subagent role")
            );
            assert!(
                requests[0]
                    .system_prompt
                    .contains("SMOKE_DELEGATE_DEPTH_N_OK")
            );
            assert_eq!(requests[0].model, ModelId::new("test"));
            assert_eq!(requests[0].thinking_level, pi_core::ThinkingLevel::Off);
            assert_eq!(
                requests[0]
                    .tools
                    .iter()
                    .map(|tool| tool.name.as_str())
                    .collect::<Vec<_>>(),
                [
                    "read",
                    "grep",
                    "find",
                    "ls",
                    "subagent",
                    "contact_supervisor",
                    "subagent_supervisor",
                    "bg_wait"
                ]
            );
        }
    }

    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn reload_rescans_markdown_agents_and_keeps_the_previous_catalog_on_failure() {
    let directory = tempfile::tempdir().unwrap();
    let agent_dir = directory.path().join("agent");
    let definitions = agent_dir.join("agents");
    std::fs::create_dir_all(&definitions).unwrap();
    let definition = definitions.join("changing.md");
    std::fs::write(
        &definition,
        "---\nname: first-agent\ndescription: First generation\n---\nInspect the first generation.",
    )
    .unwrap();
    let loader_options = SubagentLoaderOptions::new(directory.path(), &agent_dir);
    let subagents = SubagentRuntime::default();
    let runtime = PiRuntime::builder()
        .provider_plugin(ScriptedProviderPlugin::scripted([]))
        .try_agent_plugin_factory({
            let subagents = subagents.clone();
            let loader_options = loader_options.clone();
            move || SubagentsPlugin::load(subagents.clone(), loader_options.clone())
        })
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            active_tools: vec!["subagent".to_string()],
            cwd: directory.path().to_path_buf(),
            ..AgentOptions::default()
        })
        .build()
        .unwrap();

    let names = |runtime: &PiRuntime| {
        runtime
            .tool_specs()
            .into_iter()
            .find(|spec| spec.name == "subagent")
            .unwrap()
            .parameters["properties"]["agent"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    assert!(names(&runtime).contains(&"first-agent".to_string()));

    std::fs::write(
        &definition,
        "---\nname: broken-agent\ndescription: Unsupported metadata\nfallbackModels: other/model\n---\nInspect.",
    )
    .unwrap();
    assert!(runtime.reload().await.is_err());
    assert!(names(&runtime).contains(&"first-agent".to_string()));

    std::fs::write(
        &definition,
        "---\nname: second-agent\ndescription: Second generation\n---\nInspect the second generation.",
    )
    .unwrap();
    runtime.reload().await.unwrap();
    let names = names(&runtime);
    assert!(!names.contains(&"first-agent".to_string()));
    assert!(names.contains(&"second-agent".to_string()));
}

#[tokio::test]
async fn child_skill_projection_honors_aliases_private_precedence_and_missing_warnings() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(directory.path().join(".pi/agents")).unwrap();
    std::fs::create_dir_all(directory.path().join(".pi/skills/shared")).unwrap();
    std::fs::create_dir_all(directory.path().join(".pi/skills/global")).unwrap();
    std::fs::create_dir_all(directory.path().join(".pi/private-skills/shared")).unwrap();
    std::fs::create_dir_all(directory.path().join(".pi/private-skills/private")).unwrap();
    std::fs::write(
        directory.path().join(".pi/agents/skilled.md"),
        "---\nname: skilled\naliases: skilled-alias\ndescription: Skill-scoped child\ntools: grep\nexcludeTools: grep\nsystemPromptMode: replace\ninheritSkills: false\nskills: shared, private, missing\nskillPath: ../private-skills\n---\nUse only the configured skills.",
    )
    .unwrap();
    std::fs::write(
        directory.path().join(".pi/skills/shared/SKILL.md"),
        "---\nname: shared\ndescription: inherited shared\n---\ninherited",
    )
    .unwrap();
    std::fs::write(
        directory.path().join(".pi/skills/global/SKILL.md"),
        "---\nname: global\ndescription: inherited global\n---\nglobal",
    )
    .unwrap();
    std::fs::write(
        directory.path().join(".pi/private-skills/shared/SKILL.md"),
        "---\nname: shared\ndescription: agent-local shared\n---\nlocal shared",
    )
    .unwrap();
    std::fs::write(
        directory.path().join(".pi/private-skills/private/SKILL.md"),
        "---\nname: private\ndescription: agent private\n---\nprivate",
    )
    .unwrap();

    let factory = TestFactory::new().with_root_agent("skilled-alias");
    let providers = Arc::clone(&factory.providers);
    let manager = MultiSessionManager::new(factory);
    let root = manager
        .create_session(directory.path(), directory.path().join("primary.jsonl"))
        .await
        .unwrap();

    let outcome = root
        .current()
        .prompt(vec![Message::User(UserMessage {
            content: vec![pi_core::ContentBlock::Text(TextContent::new(
                "Delegate with private skills",
            ))],
            timestamp_ms: 0,
        })])
        .await
        .unwrap();

    let result = outcome
        .new_messages
        .iter()
        .find_map(|message| match message {
            Message::ToolResult(result) if result.tool_name == "subagent" => Some(result),
            _ => None,
        })
        .expect("parent should receive the child result");
    assert!(result.content.iter().any(|content| {
        matches!(content, pi_core::ContentBlock::Text(text) if text.text.contains("configured skill \"missing\" was not found"))
    }));

    let child_provider = providers
        .lock()
        .unwrap()
        .iter()
        .find_map(|(depth, provider)| (*depth == 1).then(|| Arc::clone(provider)))
        .expect("child provider should be recorded");
    let requests = child_provider.requests();
    assert_eq!(requests.len(), 1);
    let prompt = &requests[0].system_prompt;
    assert!(prompt.contains("Use only the configured skills."));
    assert!(!prompt.contains("Delegated subagent role"));
    assert!(prompt.contains("<name>shared</name>"));
    assert!(prompt.contains("agent-local shared"));
    assert!(prompt.contains("<name>private</name>"));
    assert!(!prompt.contains("inherited shared"));
    assert!(!prompt.contains("<name>global</name>"));
    assert_eq!(
        requests[0]
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        vec!["read", "contact_supervisor"]
    );

    manager.shutdown().await.unwrap();
}
