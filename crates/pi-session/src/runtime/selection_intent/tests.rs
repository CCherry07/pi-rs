#[path = "classified_tests.rs"]
mod classified_tests;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use pi_agent::{AgentOptions, EventError};
use pi_core::{AgentEvent, PluginId, ToolCallId, ToolResult, ToolSpec};
use pi_plugin::{Plugin, RegisterContext, Tool, ToolContext, ToolError, ToolUpdateSink};
use pi_runtime::PiRuntime;
use pi_test_support::{ScriptedProvider, ScriptedProviderPlugin, ScriptedTurn};
use serde_json::{Value, json};

use crate::{
    ActiveToolsEntry, AgentSession, AgentSessionOptions, CreateSessionOptions, ForkPosition,
    ModelChangeEntry, MultiSessionManager, PreparedSessionGeneration, SessionEntry,
    SessionGenerationRequest, SessionHeader, SessionStore, ThinkingLevelEntry,
};

struct Tools(bool);
struct NamedTool(&'static str);

#[pi_plugin::plugin]
impl Plugin for Tools {
    fn id(&self) -> PluginId {
        PluginId::new("selection-tools")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_tool(Arc::new(NamedTool("known")))?;
        context.register_tool(Arc::new(NamedTool("extra")))?;
        if self.0 {
            context.register_tool(Arc::new(NamedTool("later")))?;
        }
        Ok(())
    }
}

#[async_trait]
impl Tool for NamedTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.0.into(),
            label: self.0.into(),
            description: self.0.into(),
            parameters: json!({"type":"object"}),
            output_schema: None,
            constrained_sampling: None,
            execution_mode: pi_core::ToolExecutionMode::Parallel,
            prompt_snippet: None,
            prompt_guidelines: Vec::new(),
        }
    }

    async fn execute(
        &self,
        _context: ToolContext,
        _id: ToolCallId,
        _input: Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        Ok(ToolResult::text(self.0))
    }
}

fn options(allowed: bool) -> AgentSessionOptions {
    AgentSessionOptions::default().temporarily_unavailable_tools(if allowed {
        vec!["later".into()]
    } else {
        Vec::new()
    })
}

fn runtime(cwd: &Path, ready: bool) -> (PiRuntime, Arc<ScriptedProvider>) {
    let provider = ScriptedProviderPlugin::scripted(vec![ScriptedTurn::Text("done".into()); 8]);
    let capture = provider.provider();
    let runtime = PiRuntime::builder()
        .workspace(pi_core::WorkspaceSpec::from_cwd(cwd))
        .plugin(Tools(ready))
        .provider_plugin(provider)
        .agent_options(AgentOptions {
            provider_id: "scripted".into(),
            model_id: "test".into(),
            active_tools: vec!["known".into()],
            ..Default::default()
        })
        .build()
        .unwrap();
    (runtime, capture)
}

fn saved(cwd: &Path, path: &Path, tools: &[&str]) {
    let store = SessionStore::create(path, SessionHeader::new("intent", cwd)).unwrap();
    store
        .append_batch([
            SessionEntry::ModelChange(ModelChangeEntry {
                provider: "scripted".into(),
                model_id: "test".into(),
            }),
            SessionEntry::ThinkingLevelChange(ThinkingLevelEntry {
                thinking_level: "off".into(),
            }),
            SessionEntry::ActiveToolsChange(ActiveToolsEntry {
                active_tool_names: tools.iter().map(|name| (*name).into()).collect(),
            }),
        ])
        .unwrap();
}

fn desired(session: &AgentSession) -> Vec<String> {
    session.store.selected_active_tools().unwrap().unwrap()
}

#[test]
fn overlay_ceiling_query_does_not_claim_tool_availability() {
    use crate::SessionGenerationOverlay;
    assert!(SessionGenerationOverlay::default().allows_tool("not_registered"));
    assert!(
        !SessionGenerationOverlay::new()
            .with_allowed_tools(Vec::new())
            .allows_tool("known")
    );
    let ceiling = SessionGenerationOverlay::new().with_allowed_tools(vec!["later".into()]);
    assert!(ceiling.allows_tool("later"));
    assert!(!ceiling.allows_tool("Later"));
    assert!(!ceiling.allows_tool("known"));
}

#[tokio::test]
async fn passive_resume_and_message_capture_preserve_desired_selection_without_declaring_absent_tools()
 {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    saved(dir.path(), &path, &["later", "known"]);
    let before = std::fs::read(&path).unwrap();
    let (runtime, provider) = runtime(dir.path(), false);
    let session = AgentSession::open(runtime, &path, options(true))
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "passive resume must not prune or rewrite v4 intent"
    );
    assert_eq!(desired(&session), ["later", "known"]);
    assert_eq!(session.snapshot().agent.active_tools, ["known"]);
    assert!(
        !session
            .catalog()
            .tool_specs()
            .iter()
            .any(|tool| tool.name == "later")
    );
    session.prompt("Use only ready tools.").await.unwrap();
    assert_eq!(desired(&session), ["later", "known"]);
    assert_eq!(
        provider.requests()[0]
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["known"]
    );
    let changes = session
        .history()
        .branch_entries()
        .unwrap()
        .into_iter()
        .filter(|record| matches!(record.entry, SessionEntry::ActiveToolsChange(_)))
        .count();
    assert_eq!(
        changes, 1,
        "passive capture must not append a pruned or reordered selection"
    );

    // A newly discovered effective name is captured without losing pending intent.
    session
        .runtime
        .set_active_tools(["known", "extra"])
        .unwrap();
    session
        .prompt("Use the expanded ready selection.")
        .await
        .unwrap();
    assert_eq!(desired(&session), ["later", "known", "extra"]);
    assert_eq!(
        provider.requests()[1]
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["known", "extra"]
    );
    session.shutdown().await;
}

#[tokio::test]
async fn exact_absent_selection_is_lazy_and_unknown_names_are_transactionally_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("draft.jsonl");
    let (runtime, _) = runtime(dir.path(), false);
    let session = AgentSession::create(runtime, &path, options(true))
        .await
        .unwrap();
    assert_eq!(
        desired(&session),
        ["known"],
        "allowance does not itself select anything"
    );
    session
        .set_active_tools(["later", "known", "later"])
        .unwrap();
    assert_eq!(desired(&session), ["later", "known"]);
    assert_eq!(session.snapshot().agent.active_tools, ["known"]);
    let leaf = session.history().leaf_id();
    assert!(session.set_active_tools(["later", "unknown"]).is_err());
    assert_eq!(session.history().leaf_id(), leaf);
    assert_eq!(desired(&session), ["later", "known"]);
    assert_eq!(session.snapshot().agent.active_tools, ["known"]);
    assert!(!path.exists());
    session.shutdown().await;
    assert!(!path.exists());
}

#[tokio::test]
async fn explicit_empty_or_omitting_pending_selection_never_resurrects_on_capture_or_readiness() {
    for selected in [vec![], vec!["known"]] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        saved(dir.path(), &path, &["later", "known"]);
        let session = AgentSession::open(runtime(dir.path(), false).0, &path, options(true))
            .await
            .unwrap();
        session.set_active_tools(selected.clone()).unwrap();
        session
            .prompt("Persist the explicit replacement.")
            .await
            .unwrap();
        assert_eq!(desired(&session), selected);
        session.shutdown().await;
        let ready = AgentSession::open(runtime(dir.path(), true).0, &path, options(false))
            .await
            .unwrap();
        assert_eq!(desired(&ready), selected);
        assert_eq!(ready.snapshot().agent.active_tools, selected);
        ready.shutdown().await;
    }
}

#[tokio::test]
async fn removed_allowance_prunes_intent_and_default_rejects_absent_names() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    saved(dir.path(), &path, &["later", "known", "unknown"]);
    let pending = AgentSession::open(runtime(dir.path(), false).0, &path, options(true))
        .await
        .unwrap();
    assert_eq!(
        desired(&pending),
        ["later", "known"],
        "allowance is exact, not a wildcard"
    );
    pending.shutdown().await;
    let removed = AgentSession::open(
        runtime(dir.path(), false).0,
        &path,
        AgentSessionOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(desired(&removed), ["known"]);
    assert!(removed.set_active_tools(["later"]).is_err());
    removed.shutdown().await;
    let ready = AgentSession::open(
        runtime(dir.path(), true).0,
        &path,
        AgentSessionOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(desired(&ready), ["known"]);
    assert_eq!(ready.snapshot().agent.active_tools, ["known"]);
    ready.shutdown().await;
}

#[tokio::test]
async fn branch_checkout_fork_and_reload_restore_branch_local_pending_intent() {
    let dir = tempfile::tempdir().unwrap();
    let mode = Arc::new(AtomicUsize::new(0));
    let manager = MultiSessionManager::new({
        let mode = mode.clone();
        move |request: SessionGenerationRequest| {
            let mode = mode.load(Ordering::Acquire);
            async move {
                Ok(PreparedSessionGeneration::new(
                    runtime(&request.cwd, mode == 0).0,
                    options(mode == 1),
                ))
            }
        }
    });
    let session = manager
        .create_session(CreateSessionOptions::new(
            dir.path(),
            dir.path().join("session.jsonl"),
        ))
        .await
        .unwrap();
    session
        .current()
        .set_active_tools(["later", "known"])
        .unwrap();
    session.current().prompt("Both selected.").await.unwrap();
    let both_leaf = session.current().history().leaf_id().unwrap();
    session.current().set_active_tools(["known"]).unwrap();
    session.current().prompt("One selected.").await.unwrap();
    let one_leaf = session.current().history().leaf_id().unwrap();

    mode.store(1, Ordering::Release);
    session.reload().await.unwrap();
    session.current().checkout(Some(&both_leaf)).await.unwrap();
    assert_eq!(desired(&session.current()), ["later", "known"]);
    assert_eq!(session.current().snapshot().agent.active_tools, ["known"]);
    session
        .current()
        .prompt("Do not forget the branch intent.")
        .await
        .unwrap();
    assert_eq!(desired(&session.current()), ["later", "known"]);
    session.reload().await.unwrap();
    assert_eq!(desired(&session.current()), ["later", "known"]);
    session.current().checkout(Some(&one_leaf)).await.unwrap();
    session
        .current()
        .prompt("This branch excluded the absent name.")
        .await
        .unwrap();
    assert_eq!(desired(&session.current()), ["known"]);

    session.current().checkout(Some(&both_leaf)).await.unwrap();
    session
        .fork_session(both_leaf, ForkPosition::At)
        .await
        .unwrap();
    assert_eq!(desired(&session.current()), ["later", "known"]);
    assert_eq!(session.current().snapshot().agent.active_tools, ["known"]);
    mode.store(0, Ordering::Release);
    session.reload().await.unwrap();
    assert_eq!(desired(&session.current()), ["later", "known"]);
    assert_eq!(
        session.current().snapshot().agent.active_tools,
        ["later", "known"]
    );
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn plugin_setter_and_live_desired_reader_preserve_exact_intent_and_current_empty() {
    let dir = tempfile::tempdir().unwrap();
    let manager = MultiSessionManager::new(|request: SessionGenerationRequest| async move {
        Ok(PreparedSessionGeneration::new(
            runtime(&request.cwd, false).0,
            options(true),
        ))
    });
    let managed = manager
        .create_session(CreateSessionOptions::new(
            dir.path(),
            dir.path().join("draft.jsonl"),
        ))
        .await
        .unwrap();
    let session = managed.current();
    use pi_plugin::SessionContextAccess;
    let context = crate::PiPluginContext::new(
        pi_plugin::PresentationMode::Print,
        false,
        crate::PluginContextBinding::new(),
    );
    context.bind_generation_session(session.clone());
    context
        .set_active_tools(vec!["later".into(), "unknown".into(), "known".into()])
        .unwrap();
    assert_eq!(session.desired_active_tools().unwrap(), ["later", "known"]);
    assert_eq!(context.active_tools().unwrap(), ["known"]);
    context.set_active_tools(Vec::new()).unwrap();
    // Simulate an effective discovery update before its message is captured.
    session.runtime.set_active_tools(["known"]).unwrap();
    assert!(session.desired_active_tools().unwrap().is_empty());
    assert!(!session.history().path().exists());
    manager.shutdown().await.unwrap();

    // Old journals can have no baseline or selected entry at the root.
    let path = dir.path().join("old.jsonl");
    saved(dir.path(), &path, &["known"]);
    let old = AgentSession::open(runtime(dir.path(), false).0, &path, options(false))
        .await
        .unwrap();
    old.checkout(None).await.unwrap();
    assert!(old.store.selected_active_tools().unwrap().is_none());
    assert_eq!(old.desired_active_tools().unwrap(), ["known"]);
    old.shutdown().await;
}

#[test]
fn historical_candidates_include_header_all_branches_lanes_and_accepted_recovery_without_selecting_them()
 {
    use crate::runtime::recovery::SessionRecoveryPlan;
    use crate::{
        AgentMessage, LaneRecordEntry, MAIN_LANE, NewLaneRecord, OperationIntent, ProvisionedEntry,
        RestoredToolSelection,
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("draft.jsonl");
    let mut header = SessionHeader::new("history-candidates", dir.path());
    header.set_initial_active_tools(vec!["initial".into(), "duplicate".into()]);
    let store = SessionStore::create_deferred(&path, header).unwrap();
    let selection = |names: &[&str]| {
        SessionEntry::ActiveToolsChange(ActiveToolsEntry {
            active_tool_names: names.iter().map(|name| (*name).into()).collect(),
        })
    };
    store
        .append_session_record(selection(&["other_branch", "duplicate"]))
        .unwrap();
    store.move_lane(MAIN_LANE, None).unwrap();
    store.create_lane("worker", None).unwrap();
    store
        .append_to_lane(selection(&["worker_only"]), "worker")
        .unwrap();
    store.append_session_record(selection(&[])).unwrap();
    let before_leaf = store.leaf_id();
    let preview = SessionRecoveryPlan::prepare(&store)
        .unwrap()
        .configuration(&store)
        .unwrap();
    assert_eq!(
        preview.active_tools,
        RestoredToolSelection::Selected(Vec::new())
    );
    assert_eq!(
        preview.historical_selected_tool_names,
        ["initial", "duplicate", "other_branch", "worker_only"]
    );
    assert_eq!(store.leaf_id(), before_leaf);
    assert!(!path.exists());

    let user = AgentMessage::from(pi_core::Message::User(pi_core::UserMessage::text(
        "interrupted",
        1,
    )));
    store
        .append_record(NewLaneRecord {
            id: "run".into(),
            lane: MAIN_LANE.into(),
            record: LaneRecordEntry::OperationStarted {
                source_leaf_id: before_leaf.clone(),
                intent: OperationIntent::Run {
                    original_prompt: vec![user.clone()],
                    initial_messages: vec![ProvisionedEntry {
                        id: "user".into(),
                        entry: SessionEntry::message(user),
                    }],
                    system_prompt_override: None,
                    resume_data: None,
                },
            },
        })
        .unwrap();
    for (id, names) in [
        ("accepted-selection", vec!["recovered", "initial"]),
        ("accepted-clear", vec![]),
    ] {
        store
            .append_record(NewLaneRecord {
                id: format!("write-{id}"),
                lane: MAIN_LANE.into(),
                record: LaneRecordEntry::WriteDeferred {
                    run_id: "run".into(),
                    target: ProvisionedEntry {
                        id: id.into(),
                        entry: selection(&names),
                    },
                },
            })
            .unwrap();
    }
    let preview = SessionRecoveryPlan::prepare(&store)
        .unwrap()
        .configuration(&store)
        .unwrap();
    assert_eq!(
        preview.active_tools,
        RestoredToolSelection::Selected(Vec::new())
    );
    assert_eq!(
        preview.historical_selected_tool_names,
        [
            "initial",
            "duplicate",
            "other_branch",
            "worker_only",
            "recovered"
        ]
    );
    assert!(store.get_entry("accepted-selection").is_none());
    assert!(store.get_entry("accepted-clear").is_none());
    assert_eq!(store.leaf_id(), before_leaf);
    assert!(!path.exists());
}

#[tokio::test]
async fn failed_run_repairs_only_effective_selection_without_overwriting_pending_intent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    saved(dir.path(), &path, &["later", "known"]);
    let session = AgentSession::open(runtime(dir.path(), false).0, &path, options(true))
        .await
        .unwrap();
    session
        .runtime
        .agent()
        .subscribe(Arc::new(|event, _signal| async move {
            if matches!(event, AgentEvent::MessageStart { .. }) {
                return Err(EventError("injected listener failure".into()));
            }
            Ok(())
        }));
    let error = session
        .prompt("Rejected before a provider call.")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("injected listener failure"), "{error}");
    assert!(
        !error.contains("restoring the saved tool selection failed"),
        "{error}"
    );
    assert_eq!(desired(&session), ["later", "known"]);
    assert_eq!(session.snapshot().agent.active_tools, ["known"]);
    session.shutdown().await;
}
