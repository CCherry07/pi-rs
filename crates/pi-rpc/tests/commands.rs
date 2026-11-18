//! Pi command behavior exercised through the public RPC dispatch boundary.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use pi_agent::AgentOptions;
use pi_core::{ModelSpec, PluginId, ThinkingLevel};
use pi_plugin::{
    Command, CommandContext, CommandError, CommandOutcome, CommandSpec, Plugin, ProviderPlugin,
    ProviderRegisterContext, RegisterContext,
};
use pi_plugin_prompts::{PromptTemplateLoaderOptions, PromptTemplatesPlugin};
use pi_plugin_skills::{SkillLoaderOptions, SkillsPlugin};
use pi_rpc::rpc::dispatch;
use pi_runtime::PiRuntime;
use pi_session::{
    AgentMessage, AgentSession, AgentSessionOptions, CompactionSettings, CreateSessionOptions,
    ModelSwitchPolicy, MultiSessionManager, OpenSessionOptions, PiSession,
    PreparedSessionGeneration, SessionEntry, SessionGenerationRequest, SessionHeader,
    SessionSetting, SessionSettingsWriter, SessionStore,
};
use pi_test_support::ScriptedProviderPlugin;
use serde_json::{Value, json};

#[derive(Clone)]
struct Models(Vec<ModelSpec>);

#[pi_plugin::provider_plugin]
impl ProviderPlugin for Models {
    fn id(&self) -> PluginId {
        PluginId::new("rpc-catalog")
    }
    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_plugin::Result<()> {
        for model in &self.0 {
            context.register_model(model.clone())?;
        }
        Ok(())
    }
}

#[derive(Default)]
struct Preferences(Mutex<Vec<SessionSetting>>);
impl SessionSettingsWriter for Preferences {
    fn write(&self, setting: SessionSetting) -> Result<(), String> {
        self.0.lock().unwrap().push(setting);
        Ok(())
    }
}

#[derive(Default)]
struct CatalogCommands;

#[pi_plugin::plugin]
impl Plugin for CatalogCommands {
    fn id(&self) -> PluginId {
        PluginId::new("rpc-commands")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_command(Arc::new(NamedCommand("zeta")))?;
        context.register_command(Arc::new(NamedCommand("alpha")))?;
        Ok(())
    }
}

struct NamedCommand(&'static str);

#[async_trait::async_trait]
impl Command for NamedCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: self.0.into(),
            description: self.0.into(),
            argument_hint: None,
        }
    }

    async fn execute(&self, _: CommandContext, _: String) -> Result<CommandOutcome, CommandError> {
        Ok(CommandOutcome::Handled)
    }
}

fn models(names: &[&str]) -> Vec<ModelSpec> {
    names
        .iter()
        .map(|name| {
            let mut model = ModelSpec::new("scripted", *name, *name, "scripted");
            model.reasoning = true;
            model
        })
        .collect()
}

fn make_manager(
    models: Vec<ModelSpec>,
    policy: ModelSwitchPolicy,
    preferences: Arc<Preferences>,
) -> MultiSessionManager {
    manager_with_plugin(models, policy, preferences, None)
}

fn manager_with_plugin(
    models: Vec<ModelSpec>,
    policy: ModelSwitchPolicy,
    preferences: Arc<Preferences>,
    plugin: Option<Arc<dyn Plugin>>,
) -> MultiSessionManager {
    MultiSessionManager::new(move |request: SessionGenerationRequest| {
        let models = models.clone();
        let policy = policy.clone();
        let preferences = preferences.clone();
        let plugin = plugin.clone();
        async move {
            let mut builder = PiRuntime::builder()
                .provider_plugin(ScriptedProviderPlugin::scripted([]))
                .provider_plugin(Models(models.clone()))
                .plugin(PromptTemplatesPlugin::load(
                    PromptTemplateLoaderOptions::new(&request.cwd, request.cwd.join("agent")),
                ))
                .plugin(SkillsPlugin::load(SkillLoaderOptions::new(
                    &request.cwd,
                    request.cwd.join("agent"),
                )))
                .plugin(CatalogCommands)
                .agent_options(AgentOptions {
                    cwd: request.cwd,
                    provider_id: "scripted".into(),
                    model_id: models[0].id.clone(),
                    thinking_level: ThinkingLevel::Medium,
                    ..AgentOptions::default()
                });
            if let Some(plugin) = plugin {
                builder = builder.plugin_arc(plugin);
            }
            let runtime = builder.build()?;
            Ok(PreparedSessionGeneration::new(
                runtime,
                AgentSessionOptions::default()
                    .compaction(CompactionSettings {
                        enabled: false,
                        keep_recent_tokens: 1,
                        ..CompactionSettings::default()
                    })
                    .model_switch(policy)
                    .settings_writer(preferences),
            ))
        }
    })
}

struct SessionControls {
    cancel: bool,
}

struct NameObserver {
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[pi_plugin::plugin]
impl Plugin for NameObserver {
    fn id(&self) -> PluginId {
        PluginId::new("rpc-name-observer")
    }

    async fn session_info_changed(
        &self,
        _: &pi_plugin::SessionPluginContext,
        _: &pi_plugin::SessionInfoChangedEvent,
    ) -> Result<(), pi_plugin::PluginError> {
        self.started.notify_one();
        self.release.notified().await;
        Ok(())
    }
}

#[tokio::test]
async fn session_name_response_does_not_wait_for_extension_observers() {
    let directory = tempfile::tempdir().unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let manager = manager_with_plugin(
        models(&["a"]),
        ModelSwitchPolicy::default(),
        Arc::default(),
        Some(Arc::new(NameObserver {
            started: started.clone(),
            release: release.clone(),
        })),
    );
    let session = create(&manager, directory.path()).await;
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        request(
            &session,
            json!({"type":"set_session_name", "name":"available immediately"}),
        ),
    )
    .await
    .unwrap();
    assert_eq!(response["success"], true);
    assert_eq!(
        success(&session, json!({"type":"get_state"})).await["sessionName"],
        "available immediately"
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), started.notified())
        .await
        .unwrap();
    release.notify_one();
    manager.shutdown().await.unwrap();
}

#[pi_plugin::plugin]
impl Plugin for SessionControls {
    fn id(&self) -> PluginId {
        PluginId::new("rpc-session-controls")
    }

    async fn session_before_switch(
        &self,
        _: &pi_plugin::SessionPluginContext,
        _: &pi_plugin::SessionBeforeSwitchEvent,
    ) -> Result<Option<pi_plugin::SessionBeforeSwitchResult>, pi_plugin::PluginError> {
        Ok(Some(pi_plugin::SessionBeforeSwitchResult {
            cancel: self.cancel,
        }))
    }

    async fn session_before_fork(
        &self,
        _: &pi_plugin::SessionPluginContext,
        _: &pi_plugin::SessionBeforeForkEvent,
    ) -> Result<Option<pi_plugin::SessionBeforeForkResult>, pi_plugin::PluginError> {
        Ok(Some(pi_plugin::SessionBeforeForkResult {
            cancel: self.cancel,
            skip_conversation_restore: false,
        }))
    }

    async fn session_before_compact(
        &self,
        _: &pi_plugin::SessionPluginContext,
        event: &pi_plugin::SessionBeforeCompactEvent,
    ) -> Result<Option<pi_plugin::SessionBeforeCompactResult>, pi_plugin::PluginError> {
        Ok(Some(pi_plugin::SessionBeforeCompactResult {
            cancel: self.cancel,
            compaction: (!self.cancel).then(|| pi_session::CompactionEntry {
                retained_entry_ids: Vec::new(),
                retained_tail: event.preparation.retained_tail.clone(),
                summary: event
                    .custom_instructions
                    .clone()
                    .unwrap_or_else(|| "summary".into()),
                tokens_before: event.preparation.tokens_before,
                details: Some(json!({"from":"plugin"})),
                usage: None,
            }),
        }))
    }
}

#[tokio::test]
async fn session_cancellation_and_manual_compaction_match_rpc_responses() {
    for cancel in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let manager = manager_with_plugin(
            models(&["a"]),
            ModelSwitchPolicy::default(),
            Arc::default(),
            Some(Arc::new(SessionControls { cancel })),
        );
        let empty = create(&manager, directory.path()).await;
        assert_eq!(
            request(&empty, json!({"type":"compact"})).await["error"],
            "Nothing to compact (session too small)"
        );
        let path = directory.path().join("history.jsonl");
        let store = SessionStore::create(
            &path,
            SessionHeader::new("control-history", directory.path()),
        )
        .unwrap();
        append(
            &store,
            json!({"role":"user","content":"first question","timestamp":1}),
        );
        append(&store, assistant("first answer", "stop"));
        let last_user = append(
            &store,
            json!({"role":"user","content":"second question","timestamp":3}),
        );
        append(&store, assistant("second answer", "stop"));
        let session = manager
            .open_session(
                pi_session::SessionSource::Store(store),
                OpenSessionOptions::default(),
            )
            .await
            .unwrap();
        let before = session.current().history().header().id;
        let compact = request(
            &session,
            json!({"type":"compact", "customInstructions":"requested summary"}),
        )
        .await;
        if cancel {
            assert_eq!(compact["error"], "Compaction cancelled");
            for command in [
                json!({"type":"fork", "entryId":last_user}),
                json!({"type":"clone"}),
                json!({"type":"new_session"}),
                json!({"type":"switch_session", "sessionPath":path}),
            ] {
                assert_eq!(success(&session, command).await, json!({"cancelled":true}));
                assert_eq!(session.current().history().header().id, before);
            }
        } else {
            assert_eq!(compact["success"], true, "{compact}");
            assert_eq!(compact["data"]["summary"], "requested summary");
            assert_eq!(compact["data"]["details"], json!({"from":"plugin"}));
            assert!(compact["data"]["firstKeptEntryId"].is_string());
            assert_eq!(
                request(&session, json!({"type":"compact"})).await["error"],
                "Already compacted"
            );
        }
        manager.shutdown().await.unwrap();
    }
}

fn export_html(_: &AgentSession, output: Option<&str>) -> Result<PathBuf, String> {
    Ok(PathBuf::from(output.unwrap_or("session.html")))
}

async fn request(session: &PiSession, command: Value) -> Value {
    dispatch(session, command, export_html).await.unwrap()
}

async fn success(session: &PiSession, command: Value) -> Value {
    let response = request(session, command).await;
    assert_eq!(response["success"], true, "{response}");
    response["data"].clone()
}

async fn create(manager: &MultiSessionManager, cwd: &Path) -> PiSession {
    manager
        .create_session(CreateSessionOptions::new(cwd, cwd.join("session.jsonl")))
        .await
        .unwrap()
}

#[tokio::test]
async fn model_commands_respect_scope_thinking_defaults_and_single_choice() {
    let directory = tempfile::tempdir().unwrap();
    let policy = ModelSwitchPolicy {
        scope_patterns: vec!["scripted/a".into(), "scripted/b:low".into()],
        default_thinking_level: Some(ThinkingLevel::High),
        model_thinking_levels: [("scripted/b".into(), ThinkingLevel::Medium)].into(),
    };
    let manager = make_manager(models(&["a", "b", "c"]), policy, Arc::default());
    let session = create(&manager, directory.path()).await;
    assert_eq!(
        success(&session, json!({"type":"get_available_models"})).await["models"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let next = success(&session, json!({"type":"cycle_model"})).await;
    assert_eq!(next["model"]["id"], "b");
    assert_eq!(next["thinkingLevel"], "low");
    assert_eq!(next["isScoped"], true);
    assert_eq!(
        success(&session, json!({"type":"cycle_model"})).await["thinkingLevel"],
        "high"
    );
    success(
        &session,
        json!({"type":"set_model","provider":"scripted","modelId":"b"}),
    )
    .await;
    assert_eq!(
        success(&session, json!({"type":"get_state"})).await["thinkingLevel"],
        "medium"
    );
    let missing = request(
        &session,
        json!({"type":"set_model","id":"missing","provider":"scripted","modelId":"unknown"}),
    )
    .await;
    assert_eq!(
        missing,
        json!({"type":"response","id":"missing","command":"set_model","success":false,"error":"Model not found: scripted/unknown"})
    );
    success(
        &session,
        json!({"type":"set_thinking_level","level":"high"}),
    )
    .await;
    assert_eq!(
        success(&session, json!({"type":"cycle_thinking_level"})).await["level"],
        "off"
    );
    assert_eq!(
        success(&session, json!({"type":"get_available_thinking_levels"})).await["levels"],
        json!(["off", "minimal", "low", "medium", "high"])
    );
    manager.shutdown().await.unwrap();

    let single = make_manager(
        models(&["only"]),
        ModelSwitchPolicy::default(),
        Arc::default(),
    );
    let session = create(&single, directory.path()).await;
    assert_eq!(
        success(&session, json!({"type":"cycle_model"})).await,
        Value::Null
    );
    single.shutdown().await.unwrap();

    let unmatched = make_manager(
        models(&["a", "b"]),
        ModelSwitchPolicy {
            scope_patterns: vec!["missing/*".into()],
            ..ModelSwitchPolicy::default()
        },
        Arc::default(),
    );
    let session = create(&unmatched, directory.path()).await;
    let cycle = success(&session, json!({"type":"cycle_model"})).await;
    assert_eq!(cycle["model"]["id"], "b");
    assert_eq!(cycle["isScoped"], false);
    unmatched.shutdown().await.unwrap();
}

#[tokio::test]
async fn command_catalog_preserves_resource_kind_and_file_provenance() {
    let directory = tempfile::tempdir().unwrap();
    let prompt_root = directory.path().join(".pi/prompts");
    let skill_root = directory.path().join(".pi/skills/review");
    std::fs::create_dir_all(&prompt_root).unwrap();
    std::fs::create_dir_all(&skill_root).unwrap();
    std::fs::write(
        prompt_root.join("explain.md"),
        "---\ndescription: Explain code\n---\nExplain $1",
    )
    .unwrap();
    std::fs::write(
        skill_root.join("SKILL.md"),
        "---\nname: review\ndescription: Review code\n---\nReview code carefully.",
    )
    .unwrap();
    let manager = make_manager(models(&["a"]), ModelSwitchPolicy::default(), Arc::default());
    let session = create(&manager, directory.path()).await;
    let result = success(&session, json!({"type":"get_commands"})).await;
    let commands = result["commands"].as_array().unwrap();
    assert_eq!(
        commands
            .iter()
            .take(4)
            .map(|command| command["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["zeta", "alpha", "explain", "skill:review"]
    );
    for (name, source, path, base) in [
        (
            "explain",
            "prompt",
            prompt_root.join("explain.md"),
            prompt_root.clone(),
        ),
        (
            "skill:review",
            "skill",
            skill_root.join("SKILL.md"),
            directory.path().join(".pi/skills"),
        ),
    ] {
        let command = commands
            .iter()
            .find(|command| command["name"] == name)
            .unwrap();
        assert_eq!(command["source"], source);
        assert_eq!(
            command["sourceInfo"],
            json!({"path":path,"source":"local","scope":"project","origin":"top-level","baseDir":base})
        );
    }
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn session_metadata_has_stable_entry_identity_and_label_timestamp() {
    let directory = tempfile::tempdir().unwrap();
    let manager = make_manager(models(&["a"]), ModelSwitchPolicy::default(), Arc::default());
    let session = create(&manager, directory.path()).await;
    let mut events = session.current().subscribe().events;
    success(
        &session,
        json!({"type":"set_session_name", "name":"checkpoint"}),
    )
    .await;
    let entries = success(&session, json!({"type":"get_entries"})).await;
    let name_entry = entries["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["type"] == "session_info")
        .unwrap()
        .clone();
    let name_id = name_entry["id"].as_str().unwrap();
    assert_eq!(name_entry["name"], "checkpoint");
    assert_eq!(entries["leafId"], name_id);
    let event = events.try_recv().unwrap();
    let pi_session::AgentSessionEvent::EntryAppended { entry } = event.event else {
        panic!("metadata must publish an entry")
    };
    assert_eq!(entry.id, name_id);
    session
        .current()
        .set_label(name_id, Some("bookmark".into()))
        .unwrap();
    let labels = success(&session, json!({"type":"get_entries", "since":name_id})).await;
    let label_entry = &labels["entries"][0];
    assert_eq!(label_entry["type"], "label");
    assert_eq!(label_entry["parentId"], name_id);
    assert_eq!(label_entry["targetId"], name_id);
    assert_eq!(label_entry["label"], "bookmark");
    let tree = success(&session, json!({"type":"get_tree"})).await;
    fn find_node<'a>(nodes: &'a Value, id: &str) -> Option<&'a Value> {
        nodes.as_array()?.iter().find_map(|node| {
            if node["entry"]["id"] == id {
                Some(node)
            } else {
                find_node(&node["children"], id)
            }
        })
    }
    let node = find_node(&tree["tree"], name_id).unwrap();
    assert_eq!(node["label"], "bookmark");
    assert_eq!(node["labelTimestamp"], label_entry["timestamp"]);
    session.current().set_label(name_id, None).unwrap();
    let tree = success(&session, json!({"type":"get_tree"})).await;
    let node = find_node(&tree["tree"], name_id).unwrap();
    assert!(node.get("label").is_none());
    assert!(node.get("labelTimestamp").is_none());
    let persisted = session.current().history().load().unwrap();
    assert!(persisted.entries.iter().any(|record| record.id == name_id));
    assert!(
        persisted
            .entries
            .iter()
            .any(|record| record.id == label_entry["id"].as_str().unwrap())
    );
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn preference_commands_persist_through_the_product_writer_and_return_empty_success() {
    let directory = tempfile::tempdir().unwrap();
    let preferences = Arc::new(Preferences::default());
    let manager = make_manager(
        models(&["a"]),
        ModelSwitchPolicy::default(),
        preferences.clone(),
    );
    let session = create(&manager, directory.path()).await;
    for command in [
        json!({"type":"set_steering_mode","mode":"all"}),
        json!({"type":"set_follow_up_mode","mode":"all"}),
        json!({"type":"set_auto_compaction","enabled":true}),
        json!({"type":"set_auto_retry","enabled":false}),
        json!({"type":"abort_retry"}),
        json!({"type":"abort_bash"}),
    ] {
        let response = request(&session, command.clone()).await;
        assert_eq!(
            response,
            json!({"type":"response","command":command["type"],"success":true})
        );
    }
    assert_eq!(preferences.0.lock().unwrap().len(), 4);
    let state = success(&session, json!({"type":"get_state"})).await;
    assert_eq!(state["steeringMode"], "all");
    assert_eq!(state["followUpMode"], "all");
    assert_eq!(state["autoCompactionEnabled"], true);
    assert_eq!(
        success(&session, json!({"type":"clear_queue"})).await,
        json!({"steering":[],"followUp":[]})
    );
    manager.shutdown().await.unwrap();
}

fn assistant(text: &str, reason: &str) -> Value {
    json!({"role":"assistant","content":[{"type":"text","text":text}],"api":"scripted","provider":"scripted","model":"a","usage":pi_core::Usage::default(),"stopReason":reason,"timestamp":2})
}

fn append(store: &SessionStore, message: Value) -> String {
    let message: AgentMessage = serde_json::from_value(message).unwrap();
    store
        .append_session_record(SessionEntry::message(message))
        .unwrap()
        .id
}

#[tokio::test]
async fn history_queries_preserve_messages_labels_and_last_reply_semantics() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("history.jsonl");
    let store =
        SessionStore::create(&path, SessionHeader::new("rpc-history", directory.path())).unwrap();
    let user = json!({"role":"user","content":"first","timestamp":1,"futureField":{"keep":true}});
    let user_id = append(&store, user.clone());
    append(
        &store,
        json!({"role":"user","content":[{"type":"image","data":"YWJj","mimeType":"image/png"}],"timestamp":1}),
    );
    append(&store, assistant("previous", "stop"));
    append(&store, assistant("  partial reply\n", "aborted"));
    let hidden_bash = json!({"role":"bashExecution","command":"pwd","output":"/repo","exitCode":0,"cancelled":false,"truncated":false,"excludeFromContext":true,"timestamp":2});
    append(&store, hidden_bash.clone());
    let opaque =
        json!({"role":"futureRole", "content":"opaque", "future":{"retained":true}, "timestamp":2});
    append(&store, opaque.clone());
    let last_id = append(&store, assistant("", "error"));
    store
        .set_label(&user_id, Some("first turn".into()))
        .unwrap();
    let manager = make_manager(models(&["a"]), ModelSwitchPolicy::default(), Arc::default());
    let session = manager
        .open_session(
            pi_session::SessionSource::Store(store),
            OpenSessionOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        success(&session, json!({"type":"get_last_assistant_text"})).await,
        json!({"text":"partial reply"})
    );
    let messages = success(&session, json!({"type":"get_messages"})).await;
    assert_eq!(messages["messages"][0], user);
    assert!(
        messages["messages"]
            .as_array()
            .unwrap()
            .contains(&hidden_bash)
    );
    assert!(messages["messages"].as_array().unwrap().contains(&opaque));
    assert_eq!(
        success(&session, json!({"type":"get_state"})).await["messageCount"],
        7
    );
    assert_eq!(
        success(&session, json!({"type":"get_fork_messages"})).await,
        json!({"messages":[{"entryId":user_id,"text":"first"}]})
    );
    let entries = success(&session, json!({"type":"get_entries","since":last_id})).await;
    assert!(
        entries["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["id"] != last_id)
    );
    let tree = success(&session, json!({"type":"get_tree"})).await;
    assert_eq!(tree["tree"][0]["label"], "first turn");
    let stats = success(&session, json!({"type":"get_session_stats"})).await;
    assert_eq!(stats["userMessages"], 2);
    assert_eq!(stats["assistantMessages"], 3);
    assert_eq!(stats["totalMessages"], 7);
    let error = request(
        &session,
        json!({"type":"get_entries","since":"missing","id":"entries"}),
    )
    .await;
    assert_eq!(error["id"], "entries");
    assert_eq!(error["error"], "Entry not found: missing");
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn session_controls_clone_non_message_entries_and_fork_only_user_messages() {
    let directory = tempfile::tempdir().unwrap();
    let manager = make_manager(models(&["a"]), ModelSwitchPolicy::default(), Arc::default());
    let session = create(&manager, directory.path()).await;
    assert_eq!(
        success(&session, json!({"type":"get_last_assistant_text"})).await,
        json!({})
    );
    assert_eq!(
        success(
            &session,
            json!({"type":"export_html","outputPath":"report.html"})
        )
        .await,
        json!({"path":"report.html"})
    );
    assert_eq!(
        request(&session, json!({"type":"set_session_name","name":" \n "})).await["success"],
        false
    );
    success(
        &session,
        json!({"type":"set_session_name","name":"  example  "}),
    )
    .await;
    assert_eq!(
        success(&session, json!({"type":"get_state"})).await["sessionName"],
        "example"
    );
    let path = directory.path().join("saved.jsonl");
    let store =
        SessionStore::create(&path, SessionHeader::new("rpc-saved", directory.path())).unwrap();
    let user = append(
        &store,
        json!({"role":"user","content":"fork text","timestamp":1}),
    );
    let assistant = append(&store, assistant("reply", "stop"));
    store
        .append_session_record(SessionEntry::ModelChange(pi_session::ModelChangeEntry {
            provider: "scripted".into(),
            model_id: "a".into(),
        }))
        .unwrap();
    assert_eq!(
        success(
            &session,
            json!({"type":"switch_session","sessionPath":path})
        )
        .await,
        json!({"cancelled":false})
    );
    assert_eq!(
        success(&session, json!({"type":"clone"})).await,
        json!({"cancelled":false})
    );
    assert_eq!(
        request(&session, json!({"type":"fork","entryId":assistant})).await["error"],
        "Invalid entry ID for forking"
    );
    assert_eq!(
        request(&session, json!({"type":"fork","entryId":"missing"})).await["error"],
        "Invalid entry ID for forking"
    );
    assert_eq!(
        success(&session, json!({"type":"fork","entryId":user})).await,
        json!({"text":"fork text","cancelled":false})
    );
    assert_eq!(
        success(
            &session,
            json!({"type":"new_session","parentSession":"parent.jsonl"})
        )
        .await,
        json!({"cancelled":false})
    );
    assert_eq!(
        session.current().history().header().metadata.unwrap()["pi.coding-agent.parentSession"],
        "parent.jsonl"
    );
    manager.shutdown().await.unwrap();
}
