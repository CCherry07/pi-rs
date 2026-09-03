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
        }
    }

    fn nested() -> Self {
        Self {
            nested: true,
            agent_paths: vec![
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../.pi/agents"),
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
        let AgentSessionRuntimeTarget::Create { cwd, path, .. } = request.target else {
            return Err(SessionError::Runtime(
                "test factory only creates sessions".to_string(),
            ));
        };
        let depth = path
            .components()
            .filter(|component| component.as_os_str() == "isolated")
            .count();
        let turns = if self.nested {
            nested_turns(depth)
        } else if depth > 0 {
            vec![ScriptedTurn::Text("child review complete".to_string())]
        } else {
            vec![
                ScriptedTurn::ToolCalls(vec![ToolCall::new(
                    ToolCallId::new("delegate-1"),
                    "subagent",
                    json!({"agent": self.root_agent, "task": "Review the parser"}),
                )]),
                ScriptedTurn::Text("parent incorporated the review".to_string()),
            ]
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
                        ["read", "grep", "find", "ls", "subagent"]
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
        let runtime = PiRuntime::builder()
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
        let prepared = AgentSession::prepare_create_with_options(
            runtime,
            path,
            AgentSessionOptions::default().plugins(
                SessionPlugins::new().plugin(SubagentsSessionPlugin::new(self.subagents.clone())),
            ),
        )
        .await?;
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
            .contains("Delegated subagent role: reviewer")
    );
    assert!(requests[0].tools.iter().any(|tool| tool.name == "subagent"));

    manager.shutdown().await.unwrap();
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
            assert!(requests[0].system_prompt.contains("role: smoke-delegate"));
            assert!(
                requests[0]
                    .system_prompt
                    .contains(&format!("depth {depth} of 6"))
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
                ["read", "grep", "find", "ls", "subagent"]
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
    assert!(prompt.contains("Delegated subagent role: skilled"));
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
        vec!["read"]
    );

    manager.shutdown().await.unwrap();
}
