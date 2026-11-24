use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_agent::AgentOptions;
use pi_core::{
    Message, PluginId, ToolCall, ToolCallId, ToolExecutionMode, ToolResult, ToolSpec,
    WorkspaceSnapshot,
};
use pi_plugin::{
    AgentPluginContext, BeforeAgentStartEvent, BeforeAgentStartPatch, MessageEndEvent,
    MessageEndPatch, Plugin, PluginError, RegisterContext, RegistriesBuilder, Tool, ToolContext,
    ToolDiscovery, ToolError, ToolExposure, ToolNamespace, ToolResultEvent, ToolResultPatch,
    ToolUpdateSink,
};
use pi_runtime::{PiRuntime, PreparedSystemPrompt, PromptContext, PromptOutput, SystemPrompt};
use pi_session::{AgentSession, AgentSessionOptions, CompactionSettings, SessionEntry};
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};
use serde_json::{Value, json};

#[derive(Clone)]
struct DiscoveryPlugin {
    observed: Arc<Mutex<Vec<Vec<String>>>>,
    patch_result: bool,
    result_error: bool,
}

#[pi_plugin::plugin]
impl Plugin for DiscoveryPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("discovery-fixture")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
        for name in ["discover", "ordinary"] {
            context.register_tool(Arc::new(DiscoveryTool {
                name,
                observed: Arc::clone(&self.observed),
                result_error: self.result_error,
            }))?;
        }
        for name in ["alpha", "beta"] {
            context.register_tool_with_discovery(
                Arc::new(DiscoveryTool {
                    name,
                    observed: Arc::clone(&self.observed),
                    result_error: false,
                }),
                ToolDiscovery {
                    origin: Default::default(),
                    exposure: ToolExposure::Deferred,
                    namespace: Some(ToolNamespace {
                        name: "fixture".into(),
                        aliases: Vec::new(),
                        description: Some("Deferred fixture tools".into()),
                        instructions: None,
                    }),
                },
            )?;
        }
        Ok(())
    }

    async fn before_agent_start(
        &self,
        _context: AgentPluginContext,
        event: BeforeAgentStartEvent,
    ) -> Result<BeforeAgentStartPatch, PluginError> {
        Ok(BeforeAgentStartPatch {
            prompt_state: None,
            prompt_contributions: Vec::new(),
            system_prompt: Some(format!("{}:hook", event.system_prompt)),
            ..BeforeAgentStartPatch::default()
        })
    }

    async fn message_end(
        &self,
        _context: AgentPluginContext,
        event: MessageEndEvent,
    ) -> Result<MessageEndPatch, PluginError> {
        if self.patch_result
            && let Message::ToolResult(result) = event.message
            && result.tool_name == "discover"
        {
            let mut result = result.as_ref().clone();
            result.added_tool_names = Some(vec![
                "beta".into(),
                "ordinary".into(),
                "missing".into(),
                "beta".into(),
            ]);
            return Ok(MessageEndPatch {
                message: Some(Message::tool_result(result)),
            });
        }
        Ok(MessageEndPatch::default())
    }
}

struct DiscoveryTool {
    name: &'static str,
    observed: Arc<Mutex<Vec<Vec<String>>>>,
    result_error: bool,
}

#[async_trait]
impl Tool for DiscoveryTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            output_schema: None,
            constrained_sampling: None,
            name: self.name.into(),
            label: self.name.into(),
            description: self.name.into(),
            parameters: json!({"type":"object"}),
            execution_mode: ToolExecutionMode::Sequential,
            prompt_snippet: None,
            prompt_guidelines: Vec::new(),
        }
    }

    async fn execute(
        &self,
        context: ToolContext,
        _id: ToolCallId,
        _input: Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        self.observed.lock().unwrap().push(
            context
                .discoverable_tools()
                .iter()
                .map(|tool| tool.spec.name.clone())
                .collect(),
        );
        let mut result = ToolResult::text("discovered");
        if self.name == "discover" {
            result.added_tool_names = Some(vec![
                "alpha".into(),
                "beta".into(),
                "ordinary".into(),
                "missing".into(),
                "alpha".into(),
            ]);
            result.is_error = self.result_error;
        }
        Ok(result)
    }
}

fn fixture(patch_result: bool, result_error: bool) -> DiscoveryPlugin {
    DiscoveryPlugin {
        observed: Arc::new(Mutex::new(Vec::new())),
        patch_result,
        result_error,
    }
}

fn runtime(cwd: &Path, plugin: DiscoveryPlugin, provider: ScriptedProviderPlugin) -> PiRuntime {
    PiRuntime::builder()
        .plugin(plugin)
        .provider_plugin(provider)
        .agent_options(AgentOptions {
            cwd: cwd.to_path_buf(),
            active_tools: vec!["discover".into()],
            ..AgentOptions::default()
        })
        .system_prompt(SystemPrompt::dynamic(|_: &WorkspaceSnapshot| {
            Ok(PreparedSystemPrompt::new(|context: PromptContext<'_>| {
                Ok(PromptOutput::new(
                    context
                        .active_tools
                        .iter()
                        .map(|tool| tool.name.as_str())
                        .collect::<Vec<_>>()
                        .join(","),
                ))
            }))
        }))
        .build()
        .unwrap()
}

fn options() -> AgentSessionOptions {
    AgentSessionOptions::default().compaction(CompactionSettings {
        enabled: false,
        ..CompactionSettings::default()
    })
}

#[test]
fn discovery_catalog_and_activation_respect_frozen_execution_scopes() {
    let (_, registry) = RegistriesBuilder::new()
        .register_plugins(vec![Arc::new(fixture(false, false))])
        .unwrap();
    assert_eq!(
        registry
            .default_tool_specs()
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["discover", "ordinary"]
    );
    assert_eq!(
        registry
            .discoverable_tools(&["beta".into()])
            .iter()
            .map(|tool| tool.spec.name.as_str())
            .collect::<Vec<_>>(),
        ["alpha"]
    );
    let scoped = registry.scope_tool_execution(&["discover".into()], "restricted test");
    assert!(scoped.discoverable_tools(&[]).is_empty());
    assert!(
        scoped
            .tool_additions(&["alpha".into(), "beta".into()], &[])
            .is_empty()
    );
    assert!(
        scoped.tool("alpha").is_some(),
        "scope retains its guarded schema"
    );
    let restricted = registry.restrict_tools(&["alpha".into()]).unwrap();
    assert_eq!(restricted.tool_infos().len(), 1);
    assert_eq!(
        restricted.tool_additions(&["alpha".into(), "beta".into()], &[]),
        ["alpha"]
    );
}

#[tokio::test]
async fn final_discoveries_change_next_request_and_persist_normal_loadouts() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("discovery.jsonl");
    let plugin = fixture(true, false);
    let observed = Arc::clone(&plugin.observed);
    let provider = ScriptedProviderPlugin::scripted([
        ScriptedTurn::ToolCalls(vec![ToolCall::new("search", "discover", json!({}))]),
        ScriptedTurn::ToolCalls(vec![ToolCall::new("use", "beta", json!({}))]),
        ScriptedTurn::Text("done".into()),
        ScriptedTurn::Text("another prompt".into()),
    ]);
    let requests = provider.provider();
    let built = runtime(directory.path(), plugin, provider);
    let session = AgentSession::create(built, &path, options()).await.unwrap();
    session.prompt("discover").await.unwrap();
    assert_eq!(session.snapshot().agent.active_tools, ["discover", "beta"]);
    assert_eq!(session.snapshot().agent.system_prompt, "discover,beta");
    let sent = requests.requests();
    assert_eq!(
        sent[0]
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["discover"]
    );
    assert_eq!(
        sent[1]
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["discover", "beta"]
    );
    assert_eq!(sent[0].system_prompt, "discover:hook");
    assert_eq!(sent[1].system_prompt, "discover:hook");
    assert_eq!(
        *observed.lock().unwrap(),
        [vec!["alpha", "beta"], vec!["alpha"]]
    );
    let document = session.history().load().unwrap();
    let loaded = document.entries.iter().position(|entry| matches!(&entry.entry, SessionEntry::ActiveToolsChange(change) if change.active_tool_names == ["discover", "beta"])).unwrap();
    assert!(
        matches!(&document.entries[loaded - 1].entry, SessionEntry::Message(message) if matches!(message.message.as_standard(), Some(Message::ToolResult(result)) if result.tool_name == "discover"))
    );
    session.prompt("again").await.unwrap();
    assert_eq!(requests.requests()[3].system_prompt, "discover,beta:hook");
    session.shutdown().await;

    let reopened = AgentSession::open(
        runtime(
            directory.path(),
            fixture(false, false),
            ScriptedProviderPlugin::scripted([]),
        ),
        &path,
        options(),
    )
    .await
    .unwrap();
    assert_eq!(reopened.snapshot().agent.active_tools, ["discover", "beta"]);
    reopened.set_active_tools(["discover"]).unwrap();
    reopened.shutdown().await;
    let reopened = AgentSession::open(
        runtime(
            directory.path(),
            fixture(false, false),
            ScriptedProviderPlugin::scripted([]),
        ),
        &path,
        options(),
    )
    .await
    .unwrap();
    assert_eq!(
        reopened.snapshot().agent.active_tools,
        ["discover"],
        "history additions do not override later explicit selection"
    );
}

#[tokio::test]
async fn failed_results_do_not_activate_deferred_tools() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = runtime(
        directory.path(),
        fixture(false, true),
        ScriptedProviderPlugin::scripted([
            ScriptedTurn::ToolCalls(vec![ToolCall::new("search", "discover", json!({}))]),
            ScriptedTurn::Text("failed".into()),
        ]),
    );
    let session = AgentSession::create(runtime, directory.path().join("failed.jsonl"), options())
        .await
        .unwrap();
    session.prompt("discover").await.unwrap();
    assert_eq!(session.snapshot().agent.active_tools, ["discover"]);
    assert_eq!(
        session
            .history()
            .load()
            .unwrap()
            .entries
            .iter()
            .filter(|entry| matches!(entry.entry, SessionEntry::ActiveToolsChange(_)))
            .count(),
        0
    );
}

#[tokio::test]
async fn sequential_searches_see_prior_discoveries_in_the_same_batch() {
    let directory = tempfile::tempdir().unwrap();
    let plugin = fixture(false, false);
    let observed = Arc::clone(&plugin.observed);
    let runtime = runtime(
        directory.path(),
        plugin,
        ScriptedProviderPlugin::scripted([
            ScriptedTurn::ToolCalls(vec![
                ToolCall::new("first", "discover", json!({})),
                ToolCall::new("second", "discover", json!({})),
            ]),
            ScriptedTurn::Text("done".into()),
        ]),
    );
    let session = AgentSession::create(runtime, directory.path().join("batch.jsonl"), options())
        .await
        .unwrap();
    session.prompt("discover twice").await.unwrap();
    assert_eq!(
        *observed.lock().unwrap(),
        [vec!["alpha", "beta"], Vec::<&str>::new()]
    );
    assert_eq!(
        session.snapshot().agent.active_tools,
        ["discover", "alpha", "beta"]
    );
    assert_eq!(
        session
            .history()
            .load()
            .unwrap()
            .entries
            .iter()
            .filter(|entry| matches!(entry.entry, SessionEntry::ActiveToolsChange(_)))
            .count(),
        1
    );
}

struct BreakJournal {
    path: std::path::PathBuf,
    backup: std::path::PathBuf,
}

#[pi_plugin::plugin]
impl Plugin for BreakJournal {
    fn id(&self) -> PluginId {
        PluginId::new("break-discovery-journal")
    }

    async fn tool_result(
        &self,
        _context: AgentPluginContext,
        event: ToolResultEvent,
    ) -> Result<ToolResultPatch, PluginError> {
        if event.tool_call.name == "discover" {
            std::fs::rename(&self.path, &self.backup).unwrap();
            std::fs::create_dir(&self.path).unwrap();
        }
        Ok(ToolResultPatch::default())
    }
}

#[tokio::test]
async fn failed_discovery_write_restores_the_saved_selection_before_settlement() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("failed-write.jsonl");
    let backup = directory.path().join("saved.jsonl");
    let provider = ScriptedProviderPlugin::scripted([
        ScriptedTurn::ToolCalls(vec![ToolCall::new("search", "discover", json!({}))]),
        ScriptedTurn::Text("must not request after failed persistence".into()),
    ]);
    let requests = provider.provider();
    let runtime = PiRuntime::builder()
        .plugin(fixture(false, false))
        .plugin(BreakJournal {
            path: path.clone(),
            backup: backup.clone(),
        })
        .provider_plugin(provider)
        .agent_options(AgentOptions {
            cwd: directory.path().into(),
            active_tools: vec!["discover".into()],
            ..AgentOptions::default()
        })
        .build()
        .unwrap();
    let session = AgentSession::create(runtime, &path, options())
        .await
        .unwrap();
    assert!(session.prompt("discover").await.is_err());
    assert_eq!(requests.requests().len(), 1);
    assert_eq!(session.snapshot().agent.active_tools, ["discover"]);
    assert_eq!(
        session
            .history()
            .load()
            .unwrap()
            .entries
            .iter()
            .filter(|entry| matches!(entry.entry, SessionEntry::ActiveToolsChange(_)))
            .count(),
        0
    );
    std::fs::remove_dir(&path).unwrap();
    std::fs::rename(&backup, &path).unwrap();
}

#[tokio::test]
async fn a_failed_discovery_prompt_refresh_is_retried_before_the_next_provider_request() {
    let directory = tempfile::tempdir().unwrap();
    let fail_render = Arc::new(AtomicBool::new(true));
    let provider = ScriptedProviderPlugin::scripted([
        ScriptedTurn::ToolCalls(vec![ToolCall::new("search", "discover", json!({}))]),
        ScriptedTurn::Text("loaded".into()),
        ScriptedTurn::Text("next".into()),
    ]);
    let requests = provider.provider();
    let runtime = PiRuntime::builder()
        .plugin(fixture(false, false))
        .provider_plugin(provider)
        .agent_options(AgentOptions {
            cwd: directory.path().into(),
            active_tools: vec!["discover".into()],
            ..AgentOptions::default()
        })
        .system_prompt(SystemPrompt::dynamic({
            let fail_render = Arc::clone(&fail_render);
            move |_: &WorkspaceSnapshot| {
                let fail_render = Arc::clone(&fail_render);
                Ok(PreparedSystemPrompt::new(
                    move |context: PromptContext<'_>| {
                        if context.active_tools.len() > 1 && fail_render.load(Ordering::SeqCst) {
                            return Err("discovery prompt fixture failure".into());
                        }
                        Ok(PromptOutput::new(
                            context
                                .active_tools
                                .iter()
                                .map(|tool| tool.name.as_str())
                                .collect::<Vec<_>>()
                                .join(","),
                        ))
                    },
                ))
            }
        }))
        .build()
        .unwrap();
    let session = AgentSession::create(runtime, directory.path().join("render.jsonl"), options())
        .await
        .unwrap();
    assert!(
        session
            .prompt("discover")
            .await
            .unwrap_err()
            .to_string()
            .contains("discovery prompt fixture failure")
    );
    assert_eq!(
        session.snapshot().agent.active_tools,
        ["discover", "alpha", "beta"],
        "the discovery batch committed before rendering failed"
    );
    assert!(session.prompt("still invalid").await.is_err());
    assert_eq!(
        requests.requests().len(),
        1,
        "a stale prompt must not reach the continuation provider"
    );
    fail_render.store(false, Ordering::SeqCst);
    session.prompt("retry after fixing renderer").await.unwrap();
    assert_eq!(
        pi_core::SystemMessage::fold(
            &requests.requests()[1].system_prompt,
            &requests.requests()[1].messages
        ),
        "discover,alpha,beta:hook"
    );
}
