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
                from_hook: None,
                system_message: None,
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
        let mut events = session.current().subscribe();
        let compact = request(
            &session,
            json!({"type":"compact", "customInstructions":"requested summary"}),
        )
        .await;
        let mut wire_events = Vec::new();
        let mut native_compaction_entry = false;
        while let Ok(event) = events.events.try_recv() {
            if let pi_session::AgentSessionEvent::EntryAppended { entry, origin } = &event.event
                && matches!(entry.entry, SessionEntry::Compaction(_))
            {
                native_compaction_entry = true;
                assert_eq!(*origin, pi_session::EntryAppendOrigin::Compaction);
            }
            if let Some(event) =
                pi_rpc::json_wire::session_event_json(event.event, &session.current()).unwrap()
            {
                wire_events.push(event);
            }
        }
        assert!(
            wire_events
                .iter()
                .all(|event| event["type"] != "entry_appended")
        );
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
            assert!(native_compaction_entry);
            let ended = wire_events
                .iter()
                .find(|event| event["type"] == "compaction_end")
                .unwrap();
            assert_eq!(ended["result"], compact["data"]);
            assert_eq!(compact["data"]["summary"], "requested summary");
            assert_eq!(compact["data"]["details"], json!({"from":"plugin"}));
            assert!(
                compact["data"]["estimatedTokensAfter"]
                    .as_u64()
                    .is_some_and(|tokens| tokens > 0),
                "{compact}"
            );
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
    let pi_session::AgentSessionEvent::EntryAppended { entry, .. } = event.event else {
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

#[tokio::test]
async fn configuration_stays_in_history_and_native_updates_without_extra_rpc_entry_events() {
    let directory = tempfile::tempdir().unwrap();
    let manager = make_manager(models(&["a"]), ModelSwitchPolicy::default(), Arc::default());
    let session = create(&manager, directory.path()).await;
    let current = session.current();
    let mut native = current.subscribe();
    let before = success(&session, json!({"type":"get_entries"})).await;
    success(
        &session,
        json!({"type":"set_model","provider":"scripted","modelId":"a"}),
    )
    .await;
    let event = native.events.recv().await.unwrap();
    let pi_session::AgentSessionEvent::EntryAppended { entry, origin } = &event.event else {
        panic!("native update missing: {:?}", event.event);
    };
    assert_eq!(*origin, pi_session::EntryAppendOrigin::Configuration);
    assert!(matches!(entry.entry, SessionEntry::ModelChange(_)));
    let model_record = entry.clone();
    let after = success(&session, json!({"type":"get_entries"})).await;
    assert_eq!(
        after["entries"].as_array().unwrap().len(),
        before["entries"].as_array().unwrap().len() + 1
    );
    assert_eq!(
        after["entries"].as_array().unwrap().last().unwrap()["id"],
        entry.id
    );
    assert_eq!(
        pi_rpc::json_wire::session_event_json(event.event, &current).unwrap(),
        None
    );
    // The adapter must use provenance, not a blanket model_change payload filter.
    let explicit = pi_session::AgentSessionEvent::EntryAppended {
        entry: model_record,
        origin: pi_session::EntryAppendOrigin::Explicit,
    };
    assert_eq!(
        pi_rpc::json_wire::session_event_json(explicit, &current)
            .unwrap()
            .unwrap()["entry"]["type"],
        "model_change"
    );

    let id = current
        .append_custom_entry("explicit", Some(json!({"keep":false})))
        .unwrap();
    let event = native.events.recv().await.unwrap();
    let wire = pi_rpc::json_wire::session_event_json(event.event, &current)
        .unwrap()
        .unwrap();
    assert_eq!(wire["type"], "entry_appended");
    assert_eq!(wire["entry"]["id"], id);
    assert_eq!(wire["entry"]["data"], json!({"keep":false}));
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn custom_message_provenance_keeps_history_and_emits_only_message_lifecycle() {
    let directory = tempfile::tempdir().unwrap();
    let manager = make_manager(models(&["a"]), ModelSwitchPolicy::default(), Arc::default());
    let session = create(&manager, directory.path()).await;
    let current = session.current();
    let mut native = current.subscribe();
    let message = pi_core::CustomMessageInput {
        custom_type: "fixture".into(),
        content: pi_core::CustomMessageContent::Text("hello".into()),
        display: false,
        details: Some(json!({"kept":false})),
    }
    .into_message(42);
    let id = current.append_custom_message(message).unwrap();
    let mut wires = Vec::new();
    let mut entries = Vec::new();
    while let Ok(event) = native.events.try_recv() {
        if let pi_session::AgentSessionEvent::EntryAppended { entry, .. } = &event.event {
            entries.push(entry.id.clone());
        }
        if let Some(wire) = pi_rpc::json_wire::session_event_json(event.event, &current).unwrap() {
            wires.push(wire);
        }
    }
    assert_eq!(
        entries,
        [id.clone()],
        "native history update remains visible"
    );
    assert_eq!(
        wires
            .iter()
            .map(|v| v["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["message_start", "message_end"]
    );
    assert_eq!(wires[0]["message"], wires[1]["message"]);
    assert_eq!(wires[1]["message"]["details"], json!({"kept":false}));
    let history = success(&session, json!({"type":"get_entries"})).await;
    assert!(
        history["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["id"] == id && entry["type"] == "custom_message")
    );
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn shell_message_provenance_keeps_history_without_an_extra_entry_notification() {
    struct Shell;
    #[pi_plugin::plugin]
    impl Plugin for Shell {
        fn id(&self) -> PluginId {
            PluginId::new("shell-fixture")
        }
        async fn user_bash(
            &self,
            _: &pi_plugin::SessionPluginContext,
            _: &pi_plugin::UserBashEvent,
        ) -> Result<Option<pi_plugin::UserBashOverride>, pi_plugin::PluginError> {
            Ok(Some(pi_plugin::UserBashOverride::Result(
                pi_plugin::UserBashResult {
                    output: "fixture output".into(),
                    exit_code: Some(0),
                    cancelled: false,
                    truncated: false,
                    full_output_path: None,
                },
            )))
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let manager = manager_with_plugin(
        models(&["a"]),
        ModelSwitchPolicy::default(),
        Arc::default(),
        Some(Arc::new(Shell)),
    );
    let session = create(&manager, directory.path()).await;
    let current = session.current();
    let mut native = current.subscribe();
    assert_eq!(
        success(&session, json!({"type":"bash", "command":"fixture"})).await["output"],
        "fixture output"
    );
    let mut entries = Vec::new();
    while let Ok(event) = native.events.try_recv() {
        if let pi_session::AgentSessionEvent::EntryAppended { entry, .. } = &event.event {
            entries.push(entry.id.clone());
        }
        assert_ne!(
            pi_rpc::json_wire::session_event_json(event.event, &current)
                .unwrap()
                .map(|value| value["type"].clone()),
            Some(json!("entry_appended"))
        );
    }
    assert_eq!(entries.len(), 1);
    let history = success(&session, json!({"type":"get_entries"})).await;
    assert!(
        history["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["id"] == entries[0] && entry["message"]["role"] == "bashExecution")
    );
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn bash_hook_error_response_uses_the_raw_typed_message() {
    struct FailingShell;
    #[pi_plugin::plugin]
    impl Plugin for FailingShell {
        fn id(&self) -> PluginId {
            PluginId::new("js-adapter")
        }
        async fn user_bash(
            &self,
            _: &pi_plugin::SessionPluginContext,
            _: &pi_plugin::UserBashEvent,
        ) -> Result<Option<pi_plugin::UserBashOverride>, pi_plugin::PluginError> {
            Err(pi_plugin::PluginError::Hook {
                plugin_id: PluginId::new("fixture"),
                hook: "user_bash",
                source_path: Some("fixture.mjs".into()),
                message: "bash boom".into(),
            })
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let manager = manager_with_plugin(
        models(&["a"]),
        ModelSwitchPolicy::default(),
        Arc::default(),
        Some(Arc::new(FailingShell)),
    );
    let session = create(&manager, directory.path()).await;
    let result = request(&session, json!({"type":"bash","command":"must not run"})).await;
    assert_eq!(result["error"], "bash boom");
    let diagnostics = session.current().catalog().plugin_diagnostics();
    assert_eq!(diagnostics.len(), 1);
    assert!(
        diagnostics[0]
            .message
            .contains("fixture failed in user_bash")
    );
    manager.shutdown().await.unwrap();
}

struct HeldRun {
    entered: Arc<tokio::sync::Notify>,
}

#[pi_plugin::plugin]
impl Plugin for HeldRun {
    fn id(&self) -> PluginId {
        PluginId::new("rpc-held-run")
    }

    async fn agent_start(
        &self,
        context: pi_plugin::AgentPluginContext,
        _: pi_plugin::AgentStartEvent,
    ) -> Result<(), pi_plugin::PluginError> {
        self.entered.notify_one();
        context.signal().wait().await;
        Ok(())
    }
}

#[tokio::test]
async fn busy_prompt_returns_the_raw_error_without_changing_contextual_display() {
    let directory = tempfile::tempdir().unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let manager = manager_with_plugin(
        models(&["a"]),
        ModelSwitchPolicy::default(),
        Arc::default(),
        Some(Arc::new(HeldRun { entered: entered.clone() })),
    );
    let session = create(&manager, directory.path()).await;
    assert_eq!(success(&session, json!({"type":"prompt", "message":"held"})).await, json!({"disposition":"started"}));
    tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified()).await.unwrap();
    let response = request(&session, json!({"type":"prompt", "message":"not queued"})).await;
    let contextual = session.current().prompt_with_streaming_behavior_and_receipt("not queued either", None, |_| {}).await.unwrap_err();
    success(&session, json!({"type":"abort"})).await;
    manager.shutdown().await.unwrap();

    let expected = "Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.";
    assert_eq!(response["success"], false);
    assert_eq!(response["error"], expected);
    assert_eq!(contextual.to_string(), format!("runtime operation failed: {expected}"));
}

#[tokio::test]
async fn all_model_response_surfaces_preserve_optional_catalog_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = models(&["a", "b"])
        .into_iter()
        .map(|model| {
            let mut value = serde_json::to_value(model).unwrap();
            value["promptCache"] = json!({"short":30.0,"long":300.0});
            value["samplingParamsByThinkingLevel"] =
                json!({"off":{"temperature":0,"custom":null},"high":{}});
            serde_json::from_value(value).unwrap()
        })
        .collect();
    let manager = make_manager(catalog, ModelSwitchPolicy::default(), Arc::default());
    let session = create(&manager, directory.path()).await;
    let assert_metadata = |model: &Value| {
        assert_eq!(model["promptCache"], json!({"short":30.0,"long":300.0}));
        assert_eq!(
            model["samplingParamsByThinkingLevel"],
            json!({"off":{"temperature":0,"custom":null},"high":{}})
        );
    };
    assert_metadata(&success(&session, json!({"type":"get_state"})).await["model"]);
    for model in success(&session, json!({"type":"get_available_models"})).await["models"]
        .as_array()
        .unwrap()
    {
        assert_metadata(model);
    }
    assert_metadata(
        &success(
            &session,
            json!({"type":"set_model","provider":"scripted","modelId":"b"}),
        )
        .await,
    );
    assert_metadata(&success(&session, json!({"type":"cycle_model"})).await["model"]);
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn last_assistant_text_matches_executed_legacy_oracle() {
    let corpus: Value = serde_json::from_str(include_str!("fixtures/rpc-last-text.json")).unwrap();
    for case in corpus["cases"].as_array().unwrap() {
        let directory = tempfile::tempdir().unwrap();
        let store = SessionStore::create(
            directory.path().join("history.jsonl"),
            SessionHeader::new("last-text", directory.path()),
        )
        .unwrap();
        for input in case["messages"].as_array().unwrap() {
            let mut message = assistant("", input["stopReason"].as_str().unwrap());
            message["content"] = input["content"].clone();
            append(&store, message);
        }
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
            case["expected"],
            "legacy case: {}",
            case["name"],
        );
        manager.shutdown().await.unwrap();
    }
}
