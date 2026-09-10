use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_agent::AgentOptions;
use pi_core::{
    Message, ModelId, PluginContext, PresentationMode, ProviderId, TextContent, ToolCall,
    ToolCallId, Usage, UserMessage,
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
    PreparedAgentSession, SessionError, SessionPlugins, aggregate_document_usage,
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
    root_thinking_level: pi_core::ThinkingLevel,
    batch_size: usize,
    supervision: bool,
    blocked_sibling: bool,
    background: bool,
    gate: Option<Arc<tokio::sync::Notify>>,
    workflow: Option<serde_json::Value>,
    workflow_children: Vec<Vec<ScriptedTurn>>,
    fail_child_prepare: Option<usize>,
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
            root_thinking_level: pi_core::ThinkingLevel::Off,
            batch_size: 1,
            supervision: false,
            blocked_sibling: false,
            background: false,
            gate: None,
            workflow: None,
            workflow_children: Vec::new(),
            fail_child_prepare: None,
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

struct ChildGate(Option<Arc<tokio::sync::Notify>>);

#[pi_core::agent_plugin]
impl pi_core::AgentPlugin for ChildGate {
    fn id(&self) -> pi_core::PluginId {
        pi_core::PluginId::new("child-gate")
    }
    async fn before_agent_start(
        &self,
        context: pi_core::AgentPluginContext,
        _event: pi_core::BeforeAgentStartEvent,
    ) -> Result<pi_core::BeforeAgentStartPatch, pi_core::PluginError> {
        if let Some(gate) = &self.0 {
            tokio::select! { _ = gate.notified() => {}, _ = context.signal().wait() => {} }
        }
        Ok(pi_core::BeforeAgentStartPatch::default())
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
        let child_index = self
            .providers
            .lock()
            .unwrap()
            .iter()
            .filter(|(depth, _)| *depth > 0)
            .count();
        if depth > 0 && self.fail_child_prepare == Some(child_index) {
            return Err(SessionError::Runtime(
                "scripted child preparation failure".into(),
            ));
        }
        let turns = if let Some(workflow) = &self.workflow {
            if depth == 0 {
                vec![
                    ScriptedTurn::ToolCalls(vec![ToolCall::new(
                        "workflow",
                        "subagent_workflow",
                        workflow.clone(),
                    )]),
                    ScriptedTurn::Text("workflow incorporated".into()),
                    ScriptedTurn::Text("workflow notification received".into()),
                    ScriptedTurn::Text("later workflow notification received".into()),
                ]
            } else {
                self.workflow_children
                    .get(child_index)
                    .cloned()
                    .unwrap_or_else(|| vec![workflow_answer("bounded child findings")])
            }
        } else if self.background && depth > 0 {
            vec![ScriptedTurn::Text("background child done".into())]
        } else if self.background && restored_log.is_some() {
            vec![ScriptedTurn::Text("background result received".into())]
        } else if self.background {
            let mut turns = vec![ScriptedTurn::ToolCalls(vec![ToolCall::new(
                "launch-background",
                "subagent",
                json!({"agent":"reviewer","task":"Wait then review","async":true}),
            )])];
            turns.extend([
                ScriptedTurn::Text("parent continues independently".into()),
                ScriptedTurn::Text("background result received".into()),
                ScriptedTurn::Text("another background event received".into()),
            ]);
            turns
        } else if self.supervision && depth > 0 {
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
            vec![ScriptedTurn::Events(vec![
                pi_core::StreamEvent::Start {
                    metadata: pi_core::ResponseMetadata::new(
                        ProviderId::new("scripted"),
                        ModelId::new("test"),
                        "scripted",
                        0,
                    ),
                },
                pi_core::StreamEvent::TextStart { content_index: 0 },
                pi_core::StreamEvent::TextDelta {
                    content_index: 0,
                    delta: "child review complete".to_string(),
                },
                pi_core::StreamEvent::TextEnd {
                    content_index: 0,
                    text_signature: None,
                },
                pi_core::StreamEvent::Done {
                    reason: pi_core::StopReason::Stop,
                    usage: Usage {
                        input: 18,
                        output: 5,
                        total_tokens: 23,
                        ..Usage::default()
                    },
                },
            ])]
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
                        self.root_thinking_level,
                        [
                            "read",
                            "grep",
                            "find",
                            "ls",
                            "subagent",
                            "subagent_workflow",
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
            .agent_plugin(ChildGate(if depth > 0 { self.gate.clone() } else { None }))
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

fn workflow_answer(text: &str) -> ScriptedTurn {
    ScriptedTurn::Events(vec![
        pi_core::StreamEvent::Start {
            metadata: pi_core::ResponseMetadata::new(
                ProviderId::new("scripted"),
                ModelId::new("test"),
                "scripted",
                0,
            ),
        },
        pi_core::StreamEvent::TextStart { content_index: 0 },
        pi_core::StreamEvent::TextDelta {
            content_index: 0,
            delta: text.into(),
        },
        pi_core::StreamEvent::TextEnd {
            content_index: 0,
            text_signature: None,
        },
        pi_core::StreamEvent::Done {
            reason: pi_core::StopReason::Stop,
            usage: Usage {
                input: 18,
                output: 5,
                total_tokens: 23,
                ..Usage::default()
            },
        },
    ])
}

#[tokio::test]
async fn workflow_advances_parallel_lanes_and_handoffs_without_parent_turns() {
    let directory = tempfile::tempdir().unwrap();
    let mut factory = TestFactory::new();
    factory.workflow = Some(json!({"stages":[
        {"key":"scan","all":[
            {"key":"runtime","agent":"reviewer","task":"Inspect runtime"},
            {"key":"ui","agent":"reviewer","task":"Inspect UI"}
        ]},
        {"key":"review","lanes":[
            {"key":"runtime","steps":[
                {"key":"first","agent":"reviewer","task":"Review runtime", "inputs":[{"from":"scan/runtime","as":"findings"}]},
                {"key":"second","agent":"reviewer","task":"Final runtime check"}
            ]},
            {"key":"ui","steps":[{"key":"first","agent":"reviewer","task":"Review UI"}]}
        ]}
    ],"maxParallelism":2}));
    let providers = factory.providers.clone();
    let manager = MultiSessionManager::new(factory);
    let root = manager
        .create_session(directory.path(), directory.path().join("parent.jsonl"))
        .await
        .unwrap();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        root.current().prompt("Run the workflow"),
    )
    .await
    .unwrap()
    .unwrap();
    let result = outcome
        .new_messages
        .iter()
        .find_map(|m| match m {
            Message::ToolResult(r) if r.tool_name == "subagent_workflow" => Some(r),
            _ => None,
        })
        .unwrap();
    assert!(!result.is_error, "{result:?}");
    let details = result.details.as_ref().unwrap();
    assert_eq!(details["state"], "completed");
    assert_eq!(details["nodes"].as_array().unwrap().len(), 5);
    assert_eq!(details["usage"]["totalTokens"], 115);
    let status = invoke_owned_tool(&root, "subagent_supervisor", json!({"action":"status"})).await;
    assert_eq!(
        status.details.as_ref().unwrap()["runs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let member = invoke_owned_tool(
        &root,
        "subagent_supervisor",
        json!({"action":"status","id":details["nodes"][0]["runId"]}),
    )
    .await;
    assert_eq!(
        member.details.as_ref().unwrap()["runs"][0]["workflowId"],
        details["runId"]
    );
    assert!(
        details["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|node| node["usage"]["totalTokens"] == 23)
    );
    assert_eq!(
        aggregate_document_usage(&root.current().log().load().unwrap()).total_tokens,
        115,
        "workflow display totals must not be charged a second time"
    );
    let journal = root.current().log().load().unwrap();
    let snapshots = journal
        .entries
        .iter()
        .filter_map(|entry| match &entry.entry {
            pi_session::SessionEntry::Custom(custom)
                if custom.custom_type == "subagent_workflow" =>
            {
                custom.data.as_ref()
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(snapshots.len(), 2);
    assert_eq!(snapshots[1]["state"], "completed");
    let providers = providers.lock().unwrap().clone();
    assert_eq!(providers.iter().filter(|(depth, _)| *depth == 1).count(), 5);
    assert!(providers.iter().all(|(depth, _)| *depth <= 1));
    assert_eq!(
        providers
            .iter()
            .find(|(depth, _)| *depth == 0)
            .unwrap()
            .1
            .requests()
            .len(),
        2
    );
    assert!(
        providers
            .iter()
            .filter(|(depth, _)| *depth == 1)
            .any(|(_, p)| {
                let text = format!("{:?}", p.requests()[0].messages);
                text.contains("findings") && text.contains("bounded child findings")
            })
    );
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn workflow_context_defaults_and_overrides_reach_child_provider_requests() {
    for override_mode in [None, Some("fresh"), Some("fork")] {
        let directory = tempfile::tempdir().unwrap();
        let agents = directory.path().join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(agents.join("forker.md"), "---\nname: forker\ndescription: fork by default\ndefaultContext: fork\ntools: read\n---\nInspect.\n").unwrap();
        let mut workflow = json!({"stages":[{"key":"inspect","all":[
            {"key":"default-fork","agent":"forker","task":"Inspect A"},
            {"key":"default-fresh","agent":"reviewer","task":"Inspect B"},
            {"key":"explicit-fork","agent":"reviewer","task":"Inspect C","context":"fork"},
            {"key":"explicit-fresh","agent":"forker","task":"Inspect D","context":"fresh"}
        ]}]});
        if let Some(mode) = override_mode {
            workflow["context"] = json!(mode);
        }
        let mut factory = TestFactory::new();
        factory.workflow = Some(workflow);
        let providers = factory.providers.clone();
        let manager = MultiSessionManager::new(factory);
        let root = manager
            .create_session(directory.path(), directory.path().join("parent.jsonl"))
            .await
            .unwrap();
        let outcome = root
            .current()
            .prompt("Confirmed requirement: preserve compatibility")
            .await
            .unwrap();
        let details = outcome
            .new_messages
            .iter()
            .find_map(|m| match m {
                Message::ToolResult(r) if r.tool_name == "subagent_workflow" => {
                    assert!(!r.is_error, "{:?}", r.content);
                    r.details.as_ref()
                }
                _ => None,
            })
            .unwrap();
        assert!(
            details.get("context").is_none(),
            "the group must not claim a single context mode"
        );
        for (index, (_, provider)) in providers
            .lock()
            .unwrap()
            .iter()
            .filter(|(depth, _)| *depth == 1)
            .enumerate()
        {
            let mode = override_mode.unwrap_or(if index % 2 == 0 { "fork" } else { "fresh" });
            assert_eq!(details["nodes"][index]["context"], mode);
            let requests = provider.requests();
            assert_eq!(
                requests[0].messages.len(),
                if mode == "fork" { 2 } else { 1 }
            );
            assert_eq!(
                format!("{:?}", requests[0].messages).contains("Confirmed requirement"),
                mode == "fork"
            );
            assert_tool_pairs(&requests[0].messages);
        }
        assert_eq!(
            aggregate_document_usage(&root.current().log().load().unwrap()).total_tokens,
            92
        );
        manager.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn background_workflow_pins_fork_history_before_parent_continues() {
    let directory = tempfile::tempdir().unwrap();
    let gate = Arc::new(tokio::sync::Notify::new());
    let mut factory = TestFactory::new();
    factory.gate = Some(gate.clone());
    factory.workflow = Some(json!({"async":true,"context":"fork","stages":[
        {"key":"first","run":{"agent":"reviewer","task":"First child"}},
        {"key":"second","run":{"agent":"reviewer","task":"Second child","inputs":[{"from":"first","as":"findings"}]}}
    ]}));
    let providers = factory.providers.clone();
    let manager = MultiSessionManager::new(factory);
    let root = manager
        .create_session(directory.path(), directory.path().join("parent.jsonl"))
        .await
        .unwrap();
    let outcome = root
        .current()
        .prompt("Original approved requirements")
        .await
        .unwrap();
    let id = outcome
        .new_messages
        .iter()
        .find_map(|m| match m {
            Message::ToolResult(r) if r.tool_name == "subagent_workflow" => r
                .details
                .as_ref()?
                .get("runId")?
                .as_str()
                .map(str::to_string),
            _ => None,
        })
        .unwrap();
    wait_workflow_state(&root, &id, &["running", "queued"]).await;
    root.current()
        .prompt("Later unrelated parent request")
        .await
        .unwrap();
    gate.notify_one();
    wait_workflow_state(&root, &id, &["completed", "running"]).await;
    gate.notify_one();
    wait_for_parent_notice(&root, "subagent-notify").await;
    let snapshot = workflow_snapshot(&root, &id).await;
    assert_eq!(
        snapshot["nodes"][0]["forkPoint"],
        snapshot["nodes"][1]["forkPoint"]
    );
    for (_, provider) in providers
        .lock()
        .unwrap()
        .iter()
        .filter(|(depth, _)| *depth == 1)
    {
        let request = &provider.requests()[0];
        let text = format!("{:?}", request.messages);
        assert!(text.contains("Original approved requirements"));
        assert!(!text.contains("Later unrelated parent request"));
        assert!(!text.contains("workflow incorporated"));
        assert_eq!(request.messages.len(), 2);
        assert_tool_pairs(&request.messages);
    }
    let requests = providers.lock().unwrap().last().unwrap().1.requests();
    assert!(format!("{:?}", requests[0].messages).contains("bounded child findings"));
    assert_eq!(
        aggregate_document_usage(&root.current().log().load().unwrap()).total_tokens,
        46
    );
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn invalid_workflow_launches_no_children() {
    let directory = tempfile::tempdir().unwrap();
    let mut factory = TestFactory::new();
    factory.workflow = Some(json!({"stages":[{"key":"review","all":[
        {"key":"valid","agent":"reviewer","task":"Review"},
        {"key":"invalid","agent":"missing","task":"Review"}
    ]}]}));
    let providers = factory.providers.clone();
    let manager = MultiSessionManager::new(factory);
    let root = manager
        .create_session(directory.path(), directory.path().join("parent.jsonl"))
        .await
        .unwrap();
    let outcome = root.current().prompt("Run workflow").await.unwrap();
    assert!(
        outcome
            .new_messages
            .iter()
            .any(|m| matches!(m, Message::ToolResult(r) if r.is_error))
    );
    assert_eq!(providers.lock().unwrap().len(), 1);
    manager.shutdown().await.unwrap();
}

async fn invoke_owned_tool(
    root: &pi_session::PiSession,
    name: &str,
    input: serde_json::Value,
) -> pi_core::ToolResult {
    let session = root.current();
    let tool = session
        .runtime()
        .agent()
        .runtime()
        .registries()
        .tool(name)
        .unwrap();
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
            ToolCallId::new("observe-workflow"),
            input,
            pi_core::ToolUpdateSink::channel().0,
        ),
    )
    .await
    .expect("owned tool must settle")
    .unwrap()
}

async fn workflow_snapshot(root: &pi_session::PiSession, id: &str) -> serde_json::Value {
    invoke_owned_tool(
        root,
        "subagent_supervisor",
        json!({"action":"status","id":id}),
    )
    .await
    .details
    .unwrap()["runs"][0]
        .clone()
}

async fn wait_workflow_state(
    root: &pi_session::PiSession,
    id: &str,
    states: &[&str],
) -> serde_json::Value {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let snapshot = workflow_snapshot(root, id).await;
            if snapshot["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n["state"].as_str().unwrap())
                .eq(states.iter().copied())
            {
                return snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("workflow must reach expected node states")
}

#[tokio::test]
async fn workflow_background_is_bounded_stage_gated_and_notifies_only_once() {
    let directory = tempfile::tempdir().unwrap();
    let gate = Arc::new(tokio::sync::Notify::new());
    let mut factory = TestFactory::new();
    factory.gate = Some(gate.clone());
    factory.workflow = Some(json!({"async":true,"maxParallelism":2,"stages":[
        {"key":"scan","all":[{"key":"a","agent":"reviewer","task":"A"},{"key":"b","agent":"reviewer","task":"B"}]},
        {"key":"finish","run":{"agent":"reviewer","task":"Finish"}}
    ]}));
    let providers = factory.providers.clone();
    let manager = MultiSessionManager::new(factory);
    let root = manager
        .create_session(directory.path(), directory.path().join("parent.jsonl"))
        .await
        .unwrap();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        root.current().prompt("Start workflow"),
    )
    .await
    .unwrap()
    .unwrap();
    let receipt = outcome
        .new_messages
        .iter()
        .find_map(|m| match m {
            Message::ToolResult(r) if r.tool_name == "subagent_workflow" => r.details.as_ref(),
            _ => None,
        })
        .unwrap();
    assert_eq!(receipt["background"], true);
    let id = receipt["runId"].as_str().unwrap();
    wait_workflow_state(&root, id, &["running", "running", "queued"]).await;
    assert_eq!(providers.lock().unwrap().len(), 3);
    gate.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let state = workflow_snapshot(&root, id).await;
            if state["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|n| n["state"] == "completed")
                .count()
                == 1
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        providers.lock().unwrap()[0].1.requests().len(),
        2,
        "one completed node must not wake the parent"
    );
    assert_eq!(
        providers.lock().unwrap().len(),
        3,
        "next stage must wait for all tails"
    );
    gate.notify_one();
    wait_workflow_state(&root, id, &["completed", "completed", "running"]).await;
    gate.notify_one();
    wait_for_parent_notice(&root, "subagent-notify").await;
    let result = invoke_owned_tool(&root, "bg_wait", json!({"id":id})).await;
    assert!(!result.is_error);
    assert_eq!(
        workflow_snapshot(&root, id).await["usage"]["totalTokens"],
        69
    );
    assert_eq!(
        aggregate_document_usage(&root.current().log().load().unwrap()).total_tokens,
        69
    );
    let snapshot = root.current().snapshot();
    assert_eq!(
        snapshot
            .agent
            .messages
            .iter()
            .filter(|m| matches!(m, Message::Custom(c) if c.custom_type == "subagent-notify"))
            .count(),
        1
    );
    assert_eq!(providers.lock().unwrap()[0].1.requests().len(), 3);
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn workflow_supervisor_attention_returns_to_the_parent_and_resumes_the_same_child() {
    let directory = tempfile::tempdir().unwrap();
    let mut factory = TestFactory::new();
    factory.workflow = Some(json!({"stages":[{"key":"review","all":[
        {"key":"decision","agent":"reviewer","task":"Ask for a decision"},
        {"key":"independent","agent":"reviewer","task":"Review independently"}
    ]}]}));
    factory.workflow_children = vec![vec![
        ScriptedTurn::ToolCalls(vec![ToolCall::new(
            "decision",
            "contact_supervisor",
            json!({"reason":"need_decision","message":"Which compatibility rule?"}),
        )]),
        workflow_answer("decision incorporated"),
    ]];
    let providers = factory.providers.clone();
    let manager = MultiSessionManager::new(factory);
    let root = manager
        .create_session(directory.path(), directory.path().join("parent.jsonl"))
        .await
        .unwrap();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        root.current().prompt("Review together"),
    )
    .await
    .unwrap()
    .unwrap();
    let receipt = outcome
        .new_messages
        .iter()
        .find_map(|m| match m {
            Message::ToolResult(r) if r.tool_name == "subagent_workflow" => r.details.as_ref(),
            _ => None,
        })
        .unwrap();
    let id = receipt["runId"].as_str().unwrap();
    let status = workflow_snapshot(&root, id).await;
    let request_id = status["pendingRequestIds"][0].as_str().unwrap();
    assert_eq!(status["activityState"], "needs_attention");
    let child_id = status["nodes"][0]["runId"].clone();
    let reply = invoke_owned_tool(
        &root,
        "subagent_supervisor",
        json!({"action":"reply","replyTo":request_id,"message":"Preserve existing behavior"}),
    )
    .await;
    assert!(!reply.is_error);
    let result = invoke_owned_tool(&root, "bg_wait", json!({"id":id})).await;
    assert!(!result.is_error);
    let final_status = wait_workflow_state(&root, id, &["completed", "completed"]).await;
    assert_eq!(final_status["nodes"][0]["runId"], child_id);
    let children = providers
        .lock()
        .unwrap()
        .iter()
        .filter(|(depth, _)| *depth == 1)
        .map(|(_, p)| p.clone())
        .collect::<Vec<_>>();
    assert_eq!(children.len(), 2);
    assert_eq!(children[0].requests().len(), 2);
    assert!(
        format!("{:?}", children[0].requests()[1].messages).contains("Preserve existing behavior")
    );
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn workflow_failure_skips_descendants_but_finishes_independent_lanes() {
    let directory = tempfile::tempdir().unwrap();
    let mut factory = TestFactory::new();
    factory.workflow_children = vec![vec![ScriptedTurn::Error("scripted failure".into())]];
    factory.workflow = Some(
        json!({"maxParallelism":2,"stages":[{"key":"review","lanes":[
            {"key":"bad","steps":[{"key":"first","agent":"reviewer","task":"Fail"},{"key":"next","agent":"reviewer","task":"Must not run"}]},
            {"key":"good","steps":[{"key":"first","agent":"reviewer","task":"Succeed"},{"key":"next","agent":"reviewer","task":"Also succeed"}]}
        ]}]}),
    );
    let providers = factory.providers.clone();
    let manager = MultiSessionManager::new(factory);
    let root = manager
        .create_session(directory.path(), directory.path().join("parent.jsonl"))
        .await
        .unwrap();
    let outcome = root
        .current()
        .prompt("Run independent lanes")
        .await
        .unwrap();
    let result = outcome
        .new_messages
        .iter()
        .find_map(|m| match m {
            Message::ToolResult(r) if r.tool_name == "subagent_workflow" => Some(r),
            _ => None,
        })
        .unwrap();
    assert!(result.is_error);
    let details = result.details.as_ref().unwrap();
    assert_eq!(details["state"], "failed");
    assert_eq!(
        details["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["state"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["failed", "skipped", "completed", "completed"]
    );
    assert_eq!(providers.lock().unwrap().len(), 4);
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn workflow_oversized_handoff_fails_explicitly_without_launching_consumer() {
    let directory = tempfile::tempdir().unwrap();
    let mut factory = TestFactory::new();
    factory.workflow_children = vec![vec![ScriptedTurn::Text("字".repeat(12000))]];
    factory.workflow = Some(json!({"stages":[
        {"key":"source","run":{"agent":"reviewer","task":"Produce output"}},
        {"key":"consumer","run":{"agent":"reviewer","task":"Consume","inputs":[{"from":"source","as":"data"}]}}
    ]}));
    let providers = factory.providers.clone();
    let manager = MultiSessionManager::new(factory);
    let root = manager
        .create_session(directory.path(), directory.path().join("parent.jsonl"))
        .await
        .unwrap();
    let outcome = root.current().prompt("Run workflow").await.unwrap();
    let details = outcome
        .new_messages
        .iter()
        .find_map(|m| match m {
            Message::ToolResult(r) if r.tool_name == "subagent_workflow" => r.details.as_ref(),
            _ => None,
        })
        .unwrap();
    assert_eq!(details["nodes"][0]["outputTruncated"], true);
    assert_eq!(details["nodes"][1]["state"], "failed");
    assert!(
        details["nodes"][1]["resultSummary"]
            .as_str()
            .unwrap()
            .contains("No truncated data")
    );
    assert_eq!(providers.lock().unwrap().len(), 2);
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn workflow_partial_startup_failure_drains_already_started_children() {
    let directory = tempfile::tempdir().unwrap();
    let mut factory = TestFactory::new();
    factory.workflow_children = vec![vec![ScriptedTurn::WaitForAbort]];
    factory.fail_child_prepare = Some(1);
    factory.workflow = Some(json!({"stages":[{"key":"batch","all":[
        {"key":"running","agent":"reviewer","task":"Wait"},
        {"key":"broken","agent":"reviewer","task":"Preparation fails"},
        {"key":"queued","agent":"reviewer","task":"Do not launch"}
    ]}]}));
    let manager = MultiSessionManager::new(factory);
    let root = manager
        .create_session(directory.path(), directory.path().join("parent.jsonl"))
        .await
        .unwrap();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        root.current().prompt("Start batch"),
    )
    .await
    .unwrap()
    .unwrap();
    let details = outcome
        .new_messages
        .iter()
        .find_map(|m| match m {
            Message::ToolResult(r) if r.tool_name == "subagent_workflow" => r.details.as_ref(),
            _ => None,
        })
        .unwrap();
    assert_eq!(details["state"], "failed");
    assert_eq!(
        details["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["state"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["cancelled", "failed", "cancelled"]
    );
    assert!(
        manager
            .sessions()
            .iter()
            .all(|session| !session.current().runtime().agent().is_running())
    );
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn workflow_cancel_drains_active_nodes_and_does_not_launch_queued_nodes() {
    let directory = tempfile::tempdir().unwrap();
    let mut factory = TestFactory::new();
    factory.workflow_children = vec![vec![ScriptedTurn::WaitForAbort]];
    factory.workflow = Some(
        json!({"async":true,"maxParallelism":1,"stages":[{"key":"batch","all":[
            {"key":"active","agent":"reviewer","task":"Wait"},
            {"key":"queued","agent":"reviewer","task":"Do not launch"}
        ]}]}),
    );
    let providers = factory.providers.clone();
    let manager = MultiSessionManager::new(factory);
    let root = manager
        .create_session(directory.path(), directory.path().join("parent.jsonl"))
        .await
        .unwrap();
    let outcome = root.current().prompt("Start batch").await.unwrap();
    let details = outcome
        .new_messages
        .iter()
        .find_map(|m| match m {
            Message::ToolResult(r) if r.tool_name == "subagent_workflow" => r.details.as_ref(),
            _ => None,
        })
        .unwrap();
    let id = details["runId"].as_str().unwrap();
    wait_workflow_state(&root, id, &["running", "queued"]).await;
    invoke_owned_tool(
        &root,
        "subagent_supervisor",
        json!({"action":"cancel","id":id}),
    )
    .await;
    let result = invoke_owned_tool(&root, "bg_wait", json!({"id":id})).await;
    assert!(result.is_error);
    assert_eq!(workflow_snapshot(&root, id).await["state"], "cancelled");
    assert_eq!(providers.lock().unwrap().len(), 2);
    tokio::time::timeout(std::time::Duration::from_secs(3), manager.shutdown())
        .await
        .unwrap()
        .unwrap();
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
    assert_eq!(
        subagent_result.details.as_ref().unwrap()["usage"]["totalTokens"],
        23
    );
    assert_eq!(
        aggregate_document_usage(&root.current().log().load().unwrap()).total_tokens,
        23
    );
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
async fn builtin_reviewer_uses_its_profiled_thinking_level_instead_of_parent_max() {
    let directory = tempfile::tempdir().unwrap();
    let mut factory = TestFactory::new().with_root_agent("reviewer");
    factory.root_thinking_level = pi_core::ThinkingLevel::Max;
    let providers = Arc::clone(&factory.providers);
    let manager = MultiSessionManager::new(factory);
    let root = manager
        .create_session(directory.path(), directory.path().join("primary.jsonl"))
        .await
        .unwrap();

    let outcome = root
        .current()
        .prompt("Delegate a quick lookup")
        .await
        .unwrap();

    let child_provider = providers
        .lock()
        .unwrap()
        .iter()
        .find_map(|(depth, provider)| (*depth == 1).then(|| Arc::clone(provider)))
        .unwrap_or_else(|| panic!("reviewer provider should be recorded; outcome: {outcome:?}"));
    assert_eq!(
        child_provider.requests()[0].thinking_level,
        pi_core::ThinkingLevel::High
    );

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
    supervisor_roundtrip().await;
}

#[tokio::test]
async fn background_launch_returns_before_child_and_notifies_once() {
    background_roundtrip(false).await;
}

#[tokio::test]
async fn nonblocking_wait_reminder_reaches_real_parent_then_child_completes() {
    background_roundtrip(true).await;
}

async fn background_roundtrip(non_blocking: bool) {
    let directory = tempfile::tempdir().unwrap();
    let gate = Arc::new(tokio::sync::Notify::new());
    let mut factory = TestFactory::new();
    factory.background = true;
    factory.gate = Some(gate.clone());
    let providers = factory.providers.clone();
    let manager = MultiSessionManager::new(factory);
    let root = manager
        .create_session(directory.path(), directory.path().join("root.jsonl"))
        .await
        .unwrap();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        root.current()
            .prompt(vec![user_message("Delegate in background")]),
    )
    .await
    .expect("async launch must not wait for child")
    .unwrap();
    let result = outcome
        .new_messages
        .iter()
        .find_map(|message| match message {
            Message::ToolResult(result) if result.tool_name == "subagent" => Some(result),
            _ => None,
        })
        .unwrap();
    assert_eq!(result.details.as_ref().unwrap()["background"], true);
    assert_eq!(result.details.as_ref().unwrap()["detached"], true);
    if non_blocking {
        // Use the actual registered tool and the generation's PiPluginContext adapter.
        let session = root.current();
        let wait = session
            .runtime()
            .agent()
            .runtime()
            .registries()
            .tool("bg_wait")
            .unwrap();
        let (_, signal) = pi_core::AbortHandle::new();
        let context = pi_core::ToolContext::with_plugin_context(
            directory.path().to_path_buf(),
            signal,
            session.runtime().context_parts(),
        );
        let id = &result.details.as_ref().unwrap()["runId"];
        let receipt = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            wait.execute(
                context,
                ToolCallId::new("observe-background"),
                json!({"id":id,"nonBlocking":true,"timeoutMs":20}),
                pi_core::ToolUpdateSink::channel().0,
            ),
        )
        .await
        .expect("registration must return while the child is gated")
        .unwrap();
        assert_eq!(receipt.details.unwrap()["armed"], true);
        wait_for_parent_notice(&root, "subagent-wait-expired").await;
        let snapshot = root.current().snapshot();
        assert!(!snapshot.agent.messages.iter().any(|message|
            matches!(message, Message::Custom(custom) if custom.custom_type == "subagent-notify")));
        let providers = providers.lock().unwrap();
        let parent = &providers.iter().find(|(depth, _)| *depth == 0).unwrap().1;
        assert_eq!(
            parent.requests().len(),
            3,
            "expiry must trigger a real parent turn"
        );
    }
    gate.notify_one();
    wait_for_parent_notice(&root, "subagent-notify").await;
    let snapshot = root.current().snapshot();
    assert_eq!(snapshot.agent.messages.iter().filter(|message| matches!(message, Message::Custom(custom) if custom.custom_type == "subagent-notify")).count(), 1);
    assert_eq!(snapshot.agent.messages.iter().filter(|message| matches!(message, Message::Custom(custom) if custom.custom_type == "subagent-wait-expired")).count(), usize::from(non_blocking));
    for (_, provider) in providers.lock().unwrap().iter() {
        for request in provider.requests() {
            assert_tool_pairs(&request.messages);
        }
    }
    manager.shutdown().await.unwrap();
}

async fn wait_for_parent_notice(root: &pi_session::PiSession, kind: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let session = root.current();
            let snapshot = session.snapshot();
            let received = snapshot.agent.messages.iter().any(
                |message| matches!(message, Message::Custom(custom) if custom.custom_type == kind),
            );
            if received && !session.runtime().agent().is_running() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("notice must reach the parent without another user turn");
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

async fn supervisor_roundtrip() {
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
