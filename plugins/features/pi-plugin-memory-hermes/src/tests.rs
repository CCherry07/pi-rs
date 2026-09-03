use super::*;
use crate::store::{FailureOptions, MemoryTarget};
use pi_core::{EphemeralSessionStatus, Message, ToolCall, ToolCallId, ToolContext, ToolUpdateSink};
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};
use serde_json::json;

fn plugin(root: &Path, config: HermesMemoryConfig) -> Arc<HermesMemoryPlugin> {
    let store =
        HermesMemoryStore::load(root.join("agent"), root, config.clone(), Vec::new()).unwrap();
    Arc::new(HermesMemoryPlugin {
        store: Arc::new(store),
        config,
        runs: Arc::new(HermesRuns::default()),
        foreground_runs: Mutex::new(HashMap::new()),
        activity: Arc::new(Mutex::new(HashMap::new())),
        live_index: Mutex::new(None),
        backfill: Mutex::new(None),
        config_warning_emitted: AtomicBool::new(false),
    })
}

fn call(name: &str, args: serde_json::Value) -> ScriptedTurn {
    ScriptedTurn::ToolCalls(vec![ToolCall::new(format!("private-{name}"), name, args)])
}
fn create() -> ScriptedTurn {
    call(
        "skill_manage",
        json!({"action":"create","name":"cargo-validation","description":"Validate Rust changes with Cargo","content":"## Procedure\\nRun cargo check."}),
    )
}

async fn session(
    root: &Path,
    plugin: Arc<HermesMemoryPlugin>,
    turns: Vec<ScriptedTurn>,
) -> (
    Arc<pi_session::AgentSession>,
    Arc<pi_test_support::ScriptedProvider>,
) {
    session_with_setup(
        root,
        plugin,
        turns,
        pi_core::SessionExecutionOrigin::User,
        None,
    )
    .await
}

struct TestCatalog(pi_core::ModelSpec);

#[pi_core::provider_plugin]
impl pi_core::ProviderPlugin for TestCatalog {
    fn id(&self) -> PluginId {
        PluginId::new("test-catalog")
    }
    fn register(&self, context: &mut pi_core::ProviderRegisterContext<'_>) -> pi_core::Result<()> {
        context.register_model(self.0.clone())
    }
}

async fn session_with_setup(
    root: &Path,
    plugin: Arc<HermesMemoryPlugin>,
    turns: Vec<ScriptedTurn>,
    origin: pi_core::SessionExecutionOrigin,
    model: Option<pi_core::ModelSpec>,
) -> (
    Arc<pi_session::AgentSession>,
    Arc<pi_test_support::ScriptedProvider>,
) {
    use pi_session::{
        AgentSession, AgentSessionOptions, PiPluginContext, PluginContextBinding, SessionPlugins,
        SessionStartReason,
    };
    let scripted = ScriptedProviderPlugin::scripted(turns);
    let provider = scripted.provider();
    let access = Arc::new(PiPluginContext::new(
        pi_core::PresentationMode::Print,
        true,
        PluginContextBinding::new(),
    ));
    let builder = pi_runtime::PiRuntime::builder()
        .execution_origin(origin)
        .agent_plugin_arc(plugin.clone())
        .agent_plugin(pi_plugin_read::ReadPlugin)
        .agent_plugin(pi_plugin_write::WritePlugin)
        .provider_plugin(scripted)
        .plugin_context(access.clone())
        .agent_options(pi_agent::AgentOptions {
            cwd: root.to_path_buf(),
            active_tools: [
                "memory",
                "skill_manage",
                "skill_view",
                "skills_list",
                "read",
                "write",
            ]
            .map(str::to_string)
            .to_vec(),
            ..pi_agent::AgentOptions::default()
        });
    let builder = if let Some(model) = model {
        builder.provider_plugin(TestCatalog(model))
    } else {
        builder
    };
    let runtime = builder.build().unwrap();
    let options = AgentSessionOptions::default().plugins(SessionPlugins::new().plugin_arc(plugin));
    let prepared =
        AgentSession::prepare_create_with_options(runtime, root.join("session.jsonl"), options)
            .await
            .unwrap();
    access.bind_generation_session(prepared.session());
    let session = prepared
        .activate(SessionStartEvent {
            reason: SessionStartReason::Startup,
            previous_session_file: None,
        })
        .await;
    (session, provider)
}

async fn settled(plugin: &HermesMemoryPlugin) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let done = plugin
                .activity
                .lock()
                .unwrap()
                .values()
                .all(|s| s.running.as_ref().is_none_or(|r| r.finished.is_aborted()));
            if done {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn automatic_review_compacts_long_history_and_keeps_the_parent_journal_unchanged() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("context.txt"), "Verify with cargo test.").unwrap();
    let plugin = plugin(
        root.path(),
        HermesMemoryConfig {
            nudge_interval: 3,
            skill_nudge_interval: 0,
            ..HermesMemoryConfig::default()
        },
    );
    let (session, provider) = session_with_setup(
        root.path(),
        plugin.clone(),
        vec![
            ScriptedTurn::Text("I will check the evidence.".into()),
            ScriptedTurn::Text("Investigation recorded.".into()),
            ScriptedTurn::Text("Task completed.".into()),
            call("read", json!({"path":"context.txt"})),
            ScriptedTurn::Text("Verified workflow and prior evidence retained.".into()),
            call(
                "memory",
                json!({"action":"add","content":"Validate Rust changes with cargo test."}),
            ),
            ScriptedTurn::Text("Memory saved.".into()),
        ],
        pi_core::SessionExecutionOrigin::User,
        Some(pi_core::ModelSpec::new(
            "scripted", "test", "Test", "scripted",
        )),
    )
    .await;
    session
        .prompt("Investigate the build workflow.")
        .await
        .unwrap();
    let large_history = "older investigation ".repeat(20_000);
    session.prompt(large_history.clone()).await.unwrap();
    session.prompt("Finish the verification.").await.unwrap();
    let parent_messages = session.runtime().agent().state().messages;
    let journal_path = root.path().join("session.jsonl");
    let parent_journal = std::fs::read(&journal_path).unwrap();
    settled(&plugin).await;

    let requests = provider.requests();
    assert_eq!(
        requests.len(),
        7,
        "three foreground requests, read, summary, memory, final"
    );
    assert_eq!(
        requests[3].messages[..parent_messages.len()],
        parent_messages
    );
    assert_eq!(requests[3].system_prompt, requests[2].system_prompt);
    assert_eq!(requests[3].tools, requests[2].tools);
    assert!(requests[4].tools.is_empty(), "the compressor has no tools");
    assert!(
        requests[4]
            .system_prompt
            .contains("Summarize historical conversation")
    );
    assert_eq!(requests[5].system_prompt, requests[3].system_prompt);
    assert_eq!(requests[5].tools, requests[3].tools);
    let first_replay = serde_json::to_string(&requests[3].messages).unwrap();
    let compacted = serde_json::to_string(&requests[5].messages).unwrap();
    assert!(first_replay.contains(&large_history));
    assert!(!compacted.contains(&large_history));
    assert!(compacted.len() < first_replay.len() / 2);
    assert!(compacted.contains("Private context summary"));
    assert!(compacted.contains("Verify with cargo test."));
    assert_eq!(
        plugin.store.entries(MemoryTarget::Memory).unwrap(),
        vec!["Validate Rust changes with cargo test."]
    );
    assert_eq!(session.runtime().agent().state().messages, parent_messages);
    assert_eq!(std::fs::read(&journal_path).unwrap(), parent_journal);
    assert_eq!(
        walkdir::WalkDir::new(root.path())
            .into_iter()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
            .count(),
        1,
        "background compaction must not materialize or rotate a session"
    );
    assert!(
        !String::from_utf8(parent_journal)
            .unwrap()
            .contains("Private context summary")
    );
    session.shutdown().await;
}

#[tokio::test]
async fn consolidation_failure_cap_spans_tool_iterations_and_resets_for_a_new_request() {
    let root = tempfile::tempdir().unwrap();
    let plugin = plugin(
        root.path(),
        HermesMemoryConfig {
            review_enabled: false,
            ..HermesMemoryConfig::default()
        },
    );
    let failed_write = || call("memory", json!({"action":"remove","old_text":"missing"}));
    let (session, provider) = session(
        root.path(),
        plugin.clone(),
        vec![
            failed_write(),
            failed_write(),
            failed_write(),
            failed_write(),
            ScriptedTurn::Text("The task is done even though memory was not saved.".into()),
            failed_write(),
            ScriptedTurn::Text("Next request completed.".into()),
        ],
    )
    .await;
    session
        .prompt("Complete the task and save a note.")
        .await
        .unwrap();
    assert_eq!(plugin.runs.len(), 0);
    assert!(plugin.foreground_runs.lock().unwrap().is_empty());
    let requests = provider.requests();
    assert_eq!(requests.len(), 5);
    let failures = requests[4]
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::ToolResult(result) if result.tool_name == "memory" => result.details.as_ref(),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(failures.len(), 4);
    assert!(
        failures[..3]
            .iter()
            .all(|result| result.get("done") != Some(&json!(true)))
    );
    let terminal = failures[3];
    assert_eq!(terminal["success"], false);
    assert_eq!(terminal["done"], true);
    assert!(terminal["error"].as_str().unwrap().contains("4 times"));
    assert!(terminal.get("current_entries").is_none());
    assert!(terminal.get("guidance").is_none());
    assert!(
        plugin
            .store
            .entries(MemoryTarget::Memory)
            .unwrap()
            .is_empty()
    );

    session.prompt("A separate request.").await.unwrap();
    let requests = provider.requests();
    let next_failure = requests[6]
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            Message::ToolResult(result) if result.tool_name == "memory" => result.details.as_ref(),
            _ => None,
        })
        .unwrap();
    assert!(next_failure.get("done") != Some(&json!(true)));
    assert!(next_failure.get("current_entries").is_some());
    session.shutdown().await;
}

#[tokio::test]
async fn subagents_keep_memory_tools_and_injection_without_autonomous_reviews() {
    let root = tempfile::tempdir().unwrap();
    let plugin = plugin(
        root.path(),
        HermesMemoryConfig {
            nudge_interval: 1,
            skill_nudge_interval: 1,
            flush_on_compact: true,
            flush_on_shutdown: true,
            ..HermesMemoryConfig::default()
        },
    );
    plugin
        .store
        .add(
            MemoryTarget::User,
            "Prefer verified results.",
            FailureOptions::default(),
        )
        .unwrap();
    let mut turns = vec![call(
        "memory",
        json!({"action":"add","content":"The workspace uses Cargo."}),
    )];
    turns.extend((0..6).map(|_| ScriptedTurn::Text("Child task completed.".into())));
    let (session, provider) = session_with_setup(
        root.path(),
        plugin.clone(),
        turns,
        pi_core::SessionExecutionOrigin::Subagent,
        None,
    )
    .await;
    for _ in 0..6 {
        session
            .prompt("Complete the delegated task.")
            .await
            .unwrap();
        settled(&plugin).await;
    }
    assert_eq!(
        provider.requests().len(),
        7,
        "subagents must never spend a background-review request"
    );
    assert!(
        provider.requests()[0]
            .system_prompt
            .contains("Prefer verified results.")
    );
    assert_eq!(
        plugin.store.entries(MemoryTarget::Memory).unwrap(),
        vec!["The workspace uses Cargo."]
    );
    session.shutdown().await;
    assert_eq!(
        provider.requests().len(),
        7,
        "opt-in lifecycle flushes must also respect execution origin"
    );
}

#[tokio::test]
async fn memory_failure_budget_is_shared_across_targets_but_not_between_invocations() {
    let root = tempfile::tempdir().unwrap();
    let plugin = plugin(
        root.path(),
        HermesMemoryConfig {
            review_enabled: false,
            memory_char_limit: 80,
            ..HermesMemoryConfig::default()
        },
    );
    let failed_add = || call("memory", json!({"action":"add","content":"x".repeat(81)}));
    let failed_replace = || {
        call(
            "memory",
            json!({"action":"replace","old_text":"missing","content":"short"}),
        )
    };
    let failed_batch = || {
        call(
            "memory",
            json!({"operations":[
                {"action":"add","content":"Tentative entry"},
                {"action":"remove","old_text":"missing"}
            ]}),
        )
    };
    let (session, provider) = session(
        root.path(),
        plugin.clone(),
        vec![
            failed_add(),
            failed_replace(),
            failed_batch(),
            failed_add(),
            ScriptedTurn::Text("User reply still delivered.".into()),
            failed_add(),
            failed_replace(),
            failed_batch(),
            call(
                "memory",
                json!({"action":"add","target":"user","content":"Uses Rust"}),
            ),
            failed_add(),
            ScriptedTurn::Text("Review finished.".into()),
        ],
    )
    .await;
    session.prompt("Finish and remember.").await.unwrap();
    assert!(
        plugin
            .store
            .entries(MemoryTarget::Memory)
            .unwrap()
            .is_empty(),
        "failed batches must stay atomic"
    );
    let request = transport::request(
        &plugin.config,
        Arc::clone(&plugin.runs),
        vec!["memory".into()],
        "Review.",
        Vec::new(),
        None,
        Duration::from_secs(5),
    )
    .unwrap();
    let result = session
        .runtime()
        .run_ephemeral(request, AbortHandle::new().1)
        .await
        .unwrap();
    assert_eq!(result.status, EphemeralSessionStatus::Completed);
    let results = result
        .messages
        .iter()
        .filter_map(|m| match m {
            Message::ToolResult(r) => r.details.as_ref(),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 5);
    for index in [0, 1, 2, 4] {
        assert_ne!(
            results[index].get("done"),
            Some(&json!(true)),
            "separate invocation and successful writes must reset the counter"
        );
        assert!(results[index].get("current_entries").is_some());
    }
    assert_eq!(results[3]["success"], true);
    assert_eq!(provider.requests().len(), 11);
    assert!(
        plugin
            .store
            .entries(MemoryTarget::Memory)
            .unwrap()
            .is_empty()
    );
    session.shutdown().await;
}

#[tokio::test]
async fn background_review_preserves_context_reads_and_writes_skills_without_session_pollution() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("context.txt"),
        "Run cargo check and cargo test",
    )
    .unwrap();
    let plugin = plugin(
        root.path(),
        HermesMemoryConfig {
            nudge_interval: 1,
            skill_nudge_interval: 2,
            ..HermesMemoryConfig::default()
        },
    );
    plugin
        .store
        .add(
            MemoryTarget::User,
            "Prefers concise replies",
            FailureOptions::default(),
        )
        .unwrap();
    let (session, provider) = session(
        root.path(),
        plugin.clone(),
        vec![
            call("read", json!({"path":"context.txt"})),
            ScriptedTurn::Text("Task complete".into()),
            call("read", json!({"path":"context.txt"})),
            call(
                "memory",
                json!({"action":"add","target":"user","content":"Uses Cargo for verification"}),
            ),
            create(),
            call(
                "write",
                json!({"path":"must-not-exist.txt","content":"blocked"}),
            ),
            ScriptedTurn::Text("Saved memory and skill".into()),
        ],
    )
    .await;
    session.prompt("Check the project").await.unwrap();
    settled(&plugin).await;
    let requests = provider.requests();
    assert_eq!(requests.len(), 7);
    assert_eq!(requests[0].system_prompt, requests[2].system_prompt);
    assert_eq!(requests[0].tools, requests[2].tools);
    assert!(
        requests[2]
            .system_prompt
            .contains("Prefers concise replies")
    );
    assert!(
        requests[2]
            .messages
            .iter()
            .any(|m| matches!(m, Message::ToolResult(r) if r.tool_name == "read"))
    );
    assert_eq!(plugin.store.entries(MemoryTarget::User).unwrap().len(), 2);
    assert_eq!(plugin.store.list_skills().unwrap().len(), 1);
    assert!(!root.path().join("must-not-exist.txt").exists());
    assert!(
        requests[6]
            .messages
            .iter()
            .any(|m| matches!(m, Message::ToolResult(r) if r.tool_name == "write" && r.is_error))
    );
    let transcript = std::fs::read_to_string(root.path().join("session.jsonl")).unwrap();
    assert!(!transcript.contains("private-memory"));
    assert!(!transcript.contains("private-skill_manage"));
    assert_eq!(
        walkdir::WalkDir::new(root.path())
            .into_iter()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
            .count(),
        1
    );
    assert!(
        !session
            .runtime()
            .agent()
            .state()
            .messages
            .iter()
            .any(|m| matches!(m, Message::ToolResult(r) if r.tool_name == "memory"))
    );
}

#[tokio::test]
async fn many_internal_turns_do_not_trigger_memory_review_before_user_request_settles() {
    let root = tempfile::tempdir().unwrap();
    let plugin = plugin(
        root.path(),
        HermesMemoryConfig {
            nudge_interval: 2,
            skill_nudge_interval: 0,
            ..HermesMemoryConfig::default()
        },
    );
    let (session, provider) = session(
        root.path(),
        plugin.clone(),
        vec![
            call("skills_list", json!({})),
            call("skills_list", json!({})),
            ScriptedTurn::Text("first".into()),
            ScriptedTurn::Text("second".into()),
            ScriptedTurn::Text("Nothing to save".into()),
        ],
    )
    .await;
    session.prompt("one").await.unwrap();
    settled(&plugin).await;
    assert_eq!(provider.requests().len(), 3);
    session.prompt("two").await.unwrap();
    settled(&plugin).await;
    assert_eq!(provider.requests().len(), 5);
}

#[tokio::test]
async fn new_user_request_cancels_the_background_provider() {
    let root = tempfile::tempdir().unwrap();
    let plugin = plugin(
        root.path(),
        HermesMemoryConfig {
            nudge_interval: 2,
            skill_nudge_interval: 0,
            ..HermesMemoryConfig::default()
        },
    );
    let (session, provider) = session(
        root.path(),
        plugin.clone(),
        vec![
            ScriptedTurn::Text("one".into()),
            ScriptedTurn::Text("two".into()),
            ScriptedTurn::WaitForAbort,
            ScriptedTurn::Text("three".into()),
        ],
    )
    .await;
    session.prompt("one").await.unwrap();
    session.prompt("two").await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while provider.requests().len() < 3 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), session.prompt("three"))
        .await
        .unwrap()
        .unwrap();
    settled(&plugin).await;
    assert_eq!(provider.requests().len(), 4);
}

async fn fork(
    root: &Path,
    plugin: Arc<HermesMemoryPlugin>,
    turns: Vec<ScriptedTurn>,
) -> pi_core::EphemeralSessionOutcome {
    let (runtime, _) = fork_runtime(root, plugin.clone(), turns);
    runtime
        .run_ephemeral(fork_request(&runtime, &plugin), AbortHandle::new().1)
        .await
        .unwrap()
}

fn fork_runtime(
    root: &Path,
    plugin: Arc<HermesMemoryPlugin>,
    turns: Vec<ScriptedTurn>,
) -> (
    pi_runtime::PiRuntime,
    Arc<pi_test_support::ScriptedProvider>,
) {
    let scripted = ScriptedProviderPlugin::scripted(turns);
    let provider = scripted.provider();
    let runtime = pi_runtime::PiRuntime::builder()
        .agent_plugin_arc(plugin)
        .agent_plugin(pi_plugin_read::ReadPlugin)
        .provider_plugin(scripted)
        .agent_options(pi_agent::AgentOptions {
            cwd: root.to_path_buf(),
            active_tools: [
                "memory",
                "skill_manage",
                "skill_view",
                "skills_list",
                "read",
            ]
            .map(str::to_string)
            .to_vec(),
            ..pi_agent::AgentOptions::default()
        })
        .build()
        .unwrap();
    (runtime, provider)
}

fn fork_request(
    runtime: &pi_runtime::PiRuntime,
    plugin: &HermesMemoryPlugin,
) -> pi_core::EphemeralSessionRequest {
    transport::request(
        &plugin.config,
        Arc::clone(&plugin.runs),
        runtime.agent().state().active_tools,
        "Review",
        Vec::new(),
        None,
        Duration::from_secs(5),
    )
    .unwrap()
}

fn failed_memory_write() -> ScriptedTurn {
    call("memory", json!({"action":"remove","old_text":"missing"}))
}

fn skill_patch(skill: &crate::skills::SkillDocument) -> ScriptedTurn {
    call(
        "skill_manage",
        json!({"action":"patch","name":skill.name,"old_string":"cargo check","new_string":"cargo test"}),
    )
}

#[tokio::test]
async fn missing_or_mismatched_review_plugins_cannot_silently_bypass_mutation_rules() {
    for missing in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let plugin = plugin(root.path(), HermesMemoryConfig::default());
        let (runtime, _) = fork_runtime(
            root.path(),
            plugin.clone(),
            vec![
                call(
                    "memory",
                    json!({"action":"add","content":"Must not persist"}),
                ),
                create(),
                ScriptedTurn::Text("done".into()),
            ],
        );
        let mut request = fork_request(&runtime, &plugin);
        request.plugins = if missing {
            Vec::new()
        } else {
            vec![Arc::new(crate::review_plugin::HermesReviewPlugin::new(
                Arc::new(HermesRuns::default()),
            ))]
        };
        let outcome = runtime
            .run_ephemeral(request, AbortHandle::new().1)
            .await
            .unwrap();
        let receipts = outcome
            .messages
            .iter()
            .filter_map(|message| match message {
                Message::ToolResult(receipt) => Some(receipt),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(receipts.len(), 2);
        assert!(receipts.iter().all(|receipt| receipt.is_error));
        assert!(
            plugin
                .store
                .entries(MemoryTarget::Memory)
                .unwrap()
                .is_empty()
        );
        assert!(plugin.store.list_skills().unwrap().is_empty());
        assert_eq!(plugin.runs.len(), 0);
    }
}

#[tokio::test]
async fn review_state_is_released_on_every_exit_path_and_does_not_authorize_the_next_review() {
    for mode in ["complete", "error", "cancel", "timeout", "drop", "reload"] {
        let root = tempfile::tempdir().unwrap();
        let plugin = plugin(root.path(), HermesMemoryConfig::default());
        fork(
            root.path(),
            plugin.clone(),
            vec![create(), ScriptedTurn::Text("done".into())],
        )
        .await;
        let skill = plugin.store.list_skills().unwrap().remove(0);
        assert_eq!(plugin.runs.len(), 0);
        let terminal = match mode {
            "complete" => ScriptedTurn::Text("done".into()),
            "error" => ScriptedTurn::Error("review provider failed".into()),
            _ => ScriptedTurn::WaitForAbort,
        };
        let (runtime, provider) = fork_runtime(
            root.path(),
            plugin.clone(),
            vec![
                failed_memory_write(),
                failed_memory_write(),
                failed_memory_write(),
                call("read", json!({"path":skill.path})),
                terminal,
            ],
        );
        let old_context = runtime.plugin_context_handle(pi_core::PluginContextScope::Base);
        let mut request = fork_request(&runtime, &plugin);
        if mode == "timeout" {
            request.timeout = Duration::from_secs(1);
        }
        let (abort, signal) = AbortHandle::new();
        let mut run = Box::pin(runtime.run_ephemeral(request, signal));
        if matches!(mode, "complete" | "error") {
            let outcome = run.await.unwrap();
            if mode == "complete" {
                assert_eq!(outcome.status, EphemeralSessionStatus::Completed);
            } else {
                assert!(matches!(outcome.status, EphemeralSessionStatus::Failed(_)));
            }
        } else {
            tokio::select! {
                result = &mut run => panic!("review ended prematurely: {result:?}"),
                () = async { while provider.requests().len() < 5 { tokio::task::yield_now().await; } } => {},
            }
            assert_eq!(plugin.runs.len(), 1);
            assert!(plugin.foreground_runs.lock().unwrap().is_empty());
            if mode == "reload" {
                runtime.reload().await.unwrap();
                assert!(old_context.access_for_adapter().is_ok());
            }
            if mode == "drop" {
                drop(run);
            } else {
                if mode != "timeout" {
                    abort.abort();
                }
                assert_eq!(
                    run.await.unwrap().status,
                    if mode == "timeout" {
                        EphemeralSessionStatus::TimedOut
                    } else {
                        EphemeralSessionStatus::Aborted
                    }
                );
            }
        }
        assert_eq!(plugin.runs.len(), 0, "{mode} retained private review state");
        if mode == "reload" {
            assert!(matches!(
                old_context.access_for_adapter(),
                Err(pi_core::PluginContextError::Retired)
            ));
        }

        let (next, _) = fork_runtime(
            root.path(),
            plugin.clone(),
            vec![
                failed_memory_write(),
                skill_patch(&skill),
                ScriptedTurn::Text("done".into()),
            ],
        );
        let mut request = fork_request(&next, &plugin);
        request.origin = "a diagnostic label, not an execution mode".into();
        let outcome = next
            .run_ephemeral(request, AbortHandle::new().1)
            .await
            .unwrap();
        assert_eq!(outcome.status, EphemeralSessionStatus::Completed);
        let receipts = outcome
            .messages
            .iter()
            .filter_map(|message| match message {
                Message::ToolResult(receipt) => Some(receipt),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_ne!(
            receipts[0].details.as_ref().unwrap().get("done"),
            Some(&json!(true)),
            "{mode} leaked the preceding failure budget"
        );
        assert!(
            receipts[1].is_error,
            "{mode} leaked a read witness or depended on the origin label"
        );
        assert!(
            receipts[1].details.as_ref().unwrap()["error"]
                .as_str()
                .unwrap()
                .contains("Read-before-write")
        );
        assert!(
            std::fs::read_to_string(&skill.path)
                .unwrap()
                .contains("cargo check")
        );
        assert_eq!(plugin.runs.len(), 0);
    }
}

#[tokio::test]
async fn overlapping_reviews_share_storage_but_not_failure_counts_or_read_witnesses() {
    let root = tempfile::tempdir().unwrap();
    let plugin = plugin(root.path(), HermesMemoryConfig::default());
    fork(
        root.path(),
        plugin.clone(),
        vec![create(), ScriptedTurn::Text("done".into())],
    )
    .await;
    let skill = plugin.store.list_skills().unwrap().remove(0);
    let (first, provider) = fork_runtime(
        root.path(),
        plugin.clone(),
        vec![
            failed_memory_write(),
            failed_memory_write(),
            failed_memory_write(),
            call("skill_view", json!({"name":skill.name})),
            ScriptedTurn::WaitForAbort,
        ],
    );
    let (abort, signal) = AbortHandle::new();
    let mut pending = Box::pin(first.run_ephemeral(fork_request(&first, &plugin), signal));
    tokio::select! {
        result = &mut pending => panic!("first review ended prematurely: {result:?}"),
        () = async { while provider.requests().len() < 5 { tokio::task::yield_now().await; } } => {},
    }
    assert_eq!(plugin.runs.len(), 1);
    let outcome = fork(
        root.path(),
        plugin.clone(),
        vec![
            failed_memory_write(),
            failed_memory_write(),
            failed_memory_write(),
            failed_memory_write(),
            skill_patch(&skill),
            call(
                "memory",
                json!({"action":"add","content":"Shared durable note."}),
            ),
            ScriptedTurn::Text("done".into()),
        ],
    )
    .await;
    let receipts = outcome
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::ToolResult(receipt) => Some(receipt),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        receipts[..3]
            .iter()
            .all(|receipt| receipt.details.as_ref().unwrap().get("done") != Some(&json!(true)))
    );
    assert_eq!(receipts[3].details.as_ref().unwrap()["done"], true);
    assert!(
        receipts[4].is_error,
        "another in-flight review's reads must not authorize this one"
    );
    assert!(!receipts[5].is_error);
    assert_eq!(
        plugin.store.entries(MemoryTarget::Memory).unwrap(),
        ["Shared durable note."]
    );
    assert_eq!(
        plugin.runs.len(),
        1,
        "finishing one review must not discard another's state"
    );
    assert!(plugin.foreground_runs.lock().unwrap().is_empty());
    abort.abort();
    assert_eq!(
        pending.await.unwrap().status,
        EphemeralSessionStatus::Aborted
    );
    assert_eq!(plugin.runs.len(), 0);
}

#[tokio::test]
async fn autonomous_skill_updates_require_ownership_and_a_fresh_read() {
    let root = tempfile::tempdir().unwrap();
    let plugin = plugin(root.path(), HermesMemoryConfig::default());
    let result = fork(
        root.path(),
        plugin.clone(),
        vec![create(), ScriptedTurn::Text("done".into())],
    )
    .await;
    assert_eq!(result.status, EphemeralSessionStatus::Completed);
    let skill = plugin.store.list_skills().unwrap().remove(0);
    assert!(skill.path.with_file_name("curator.json").exists());
    let patch = || {
        call(
            "skill_manage",
            json!({"action":"patch","name":skill.name,"old_string":"cargo check","new_string":"cargo test"}),
        )
    };
    let result = fork(
        root.path(),
        plugin.clone(),
        vec![
            patch(),
            call("read", json!({"path":skill.path})),
            patch(),
            ScriptedTurn::Text("done".into()),
        ],
    )
    .await;
    let results = result
        .messages
        .iter()
        .filter_map(|m| match m {
            Message::ToolResult(r) => Some(r),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(results[0].is_error);
    assert!(!results[1].is_error);
    assert!(!results[2].is_error);
    assert!(
        std::fs::read_to_string(&skill.path)
            .unwrap()
            .contains("cargo test")
    );
    let unowned = plugin
        .store
        .create_skill(crate::skills::SkillCreate {
            scope: crate::skills::SkillScope::Global,
            name: "user-owned".into(),
            description: "A manually maintained unrelated procedure".into(),
            body: "Keep this content".into(),
        })
        .unwrap();
    let result = fork(
        root.path(),
        plugin.clone(),
        vec![
            call("read", json!({"path":unowned.path})),
            call(
                "skill_manage",
                json!({"action":"edit","name":"user-owned","content":"changed"}),
            ),
            ScriptedTurn::Text("done".into()),
        ],
    )
    .await;
    assert!(result.messages.iter().any(
        |m| matches!(m, Message::ToolResult(r) if r.tool_name == "skill_manage" && r.is_error)
    ));
    assert!(
        std::fs::read_to_string(&unowned.path)
            .unwrap()
            .contains("Keep this content")
    );
}

#[tokio::test]
async fn unified_memory_batches_are_atomic_and_overflow_stays_in_current_agent() {
    let root = tempfile::tempdir().unwrap();
    let plugin = plugin(
        root.path(),
        HermesMemoryConfig {
            memory_char_limit: 80,
            ..HermesMemoryConfig::default()
        },
    );
    let runtime = pi_runtime::PiRuntime::builder()
        .agent_plugin_arc(plugin.clone())
        .provider_plugin(ScriptedProviderPlugin::scripted([]))
        .build()
        .unwrap();
    let tool = runtime
        .agent()
        .runtime()
        .registries()
        .tool("memory")
        .unwrap();
    let execute = |input| {
        let tool = tool.clone();
        let cwd = root.path().to_path_buf();
        async move {
            tool.execute(
                ToolContext::standalone(cwd, AbortHandle::new().1),
                ToolCallId::new("memory"),
                input,
                ToolUpdateSink::channel().0,
            )
            .await
            .unwrap()
        }
    };
    assert!(
        !execute(json!({"action":"add","content":"prefers short answers"}))
            .await
            .is_error
    );
    let before = plugin.store.entries(MemoryTarget::Memory).unwrap();
    assert!(execute(json!({"operations":[{"action":"remove","old_text":"prefers"},{"action":"add","content":"x".repeat(100)}]})).await.is_error);
    assert_eq!(before, plugin.store.entries(MemoryTarget::Memory).unwrap());
    let failed = execute(json!({"action":"add","content":"x".repeat(100)})).await;
    assert!(failed.is_error);
    assert!(failed.details.unwrap().get("current_entries").is_some());
    assert!(!execute(json!({"operations":[{"action":"remove","old_text":"prefers"},{"action":"add","content":"concise replies"}]})).await.is_error);
}

#[tokio::test]
async fn frozen_memory_changes_only_for_a_new_session_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let plugin = plugin(
        root.path(),
        HermesMemoryConfig {
            review_enabled: false,
            ..HermesMemoryConfig::default()
        },
    );
    plugin
        .store
        .add(
            MemoryTarget::User,
            "Original preference",
            FailureOptions::default(),
        )
        .unwrap();
    let (session, provider) = session(
        root.path(),
        plugin.clone(),
        vec![
            ScriptedTurn::Text("one".into()),
            ScriptedTurn::Text("two".into()),
        ],
    )
    .await;
    session.prompt("one").await.unwrap();
    plugin
        .store
        .add(
            MemoryTarget::User,
            "New preference",
            FailureOptions::default(),
        )
        .unwrap();
    session.prompt("two").await.unwrap();
    assert_eq!(
        provider.requests()[0].system_prompt,
        provider.requests()[1].system_prompt
    );
    assert!(
        !provider.requests()[1]
            .system_prompt
            .contains("New preference")
    );
    plugin.store.start_session(root.path()).unwrap();
    assert!(
        plugin
            .store
            .legacy_global_context()
            .contains("New preference")
    );
}

#[tokio::test]
async fn foreground_tool_created_skills_are_curatable_but_pinned_skills_are_not() {
    let root = tempfile::tempdir().unwrap();
    let plugin = plugin(
        root.path(),
        HermesMemoryConfig {
            nudge_interval: 0,
            skill_nudge_interval: 1,
            ..HermesMemoryConfig::default()
        },
    );
    let (session, _) = session(root.path(), plugin.clone(), vec![create(), ScriptedTurn::Text("created".into()),
        call("skill_view", json!({"name":"cargo-validation"})),
        call("skill_manage", json!({"action":"patch","name":"cargo-validation","old_string":"cargo check","new_string":"cargo test"})),
        ScriptedTurn::Text("reviewed".into()),
    ]).await;
    session
        .prompt("Save this reusable procedure")
        .await
        .unwrap();
    settled(&plugin).await;
    let skill = plugin.store.list_skills().unwrap().remove(0);
    assert!(
        std::fs::read_to_string(&skill.path)
            .unwrap()
            .contains("cargo test")
    );
    let meta = skill.path.with_file_name("curator.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&meta).unwrap()).unwrap();
    value["pinned"] = json!(true);
    std::fs::write(meta, value.to_string()).unwrap();
    let result = fork(
        root.path(),
        plugin.clone(),
        vec![
            call("skill_view", json!({"name":skill.name})),
            call(
                "skill_manage",
                json!({"action":"edit","name":skill.name,"content":"must not change"}),
            ),
            ScriptedTurn::Text("done".into()),
        ],
    )
    .await;
    assert!(result.messages.iter().any(
        |m| matches!(m, Message::ToolResult(r) if r.tool_name == "skill_manage" && r.is_error)
    ));
    assert!(
        transport::action_summary(&result.messages).is_empty(),
        "Reads and failed writes must not be reported as mutations"
    );
}

#[test]
fn memory_budget_counts_unicode_characters_and_replacement_uses_a_unique_substring() {
    let root = tempfile::tempdir().unwrap();
    let plugin = plugin(
        root.path(),
        HermesMemoryConfig {
            memory_char_limit: 5,
            ..HermesMemoryConfig::default()
        },
    );
    assert!(
        plugin
            .store
            .add(
                MemoryTarget::Memory,
                "😀😀😀😀😀",
                FailureOptions::default()
            )
            .unwrap()
            .success
    );
    assert!(
        plugin
            .store
            .replace(MemoryTarget::Memory, "😀", "甲\n乙")
            .unwrap()
            .success
    );
    assert!(
        plugin
            .store
            .replace(MemoryTarget::Memory, "甲", "丙")
            .unwrap()
            .success
    );
    assert_eq!(plugin.store.entries(MemoryTarget::Memory).unwrap(), ["丙"]);
}
