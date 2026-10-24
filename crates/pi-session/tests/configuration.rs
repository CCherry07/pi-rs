use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use pi_agent::AgentOptions;
use pi_core::{ModelId, ModelSpec, PluginId, ProviderId, ThinkingLevel, WorkspaceSnapshot};
use pi_plugin::{ProviderPlugin, ProviderRegisterContext};
use pi_runtime::{
    PiRuntime, PiRuntimeBuilder, PreparedSystemPrompt, PromptContext, PromptOutput, SystemPrompt,
};
use pi_session::{
    ActiveToolsEntry, AgentSession, AgentSessionEvent, AgentSessionOptions, AgentSessionSnapshot,
    CompactionSettings, ModelChangeEntry, MultiSessionManager, PreparedSessionGeneration,
    SessionDocument, SessionEntry, SessionError, SessionGenerationRequest, SessionHeader,
    SessionStore, ThinkingLevelEntry,
};
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn, TestToolsPlugin};
use serde_json::{Value, json};

struct ConfigurationCatalog;

#[pi_plugin::provider_plugin]
impl ProviderPlugin for ConfigurationCatalog {
    fn id(&self) -> PluginId {
        PluginId::new("configuration-catalog")
    }

    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_plugin::Result<()> {
        let mut reasoning = ModelSpec::new("scripted", "reasoning", "Reasoning", "scripted");
        reasoning.reasoning = true;
        context.register_model(reasoning)?;
        context.register_model(ModelSpec::new("scripted", "plain", "Plain", "scripted"))
    }
}

fn runtime_builder(cwd: &Path, provider: ScriptedProviderPlugin) -> PiRuntimeBuilder {
    PiRuntime::builder()
        .provider_plugin(provider)
        .provider_plugin(ConfigurationCatalog)
        .plugin(TestToolsPlugin::new())
        .agent_options(AgentOptions {
            cwd: cwd.to_path_buf(),
            model_id: ModelId::new("reasoning"),
            thinking_level: ThinkingLevel::High,
            active_tools: vec!["echo".into()],
            ..AgentOptions::default()
        })
        .system_prompt(SystemPrompt::dynamic(|_: &WorkspaceSnapshot| {
            Ok(PreparedSystemPrompt::new(|context: PromptContext<'_>| {
                let tools = context
                    .active_tools
                    .iter()
                    .map(|tool| tool.name.as_str())
                    .collect::<Vec<_>>();
                if tools.contains(&"fail") {
                    return Err("fixture prompt rendering failed".into());
                }
                Ok(PromptOutput {
                    system_prompt: tools.join(","),
                    options: Some(json!({ "tools": tools })),
                })
            }))
        }))
}

fn options() -> AgentSessionOptions {
    AgentSessionOptions::default().compaction(CompactionSettings {
        enabled: false,
        ..CompactionSettings::default()
    })
}

fn saved_log(path: &Path, cwd: &Path, id: &str) {
    let log = SessionStore::create(path, SessionHeader::new(id, cwd)).unwrap();
    log.append_batch([
        SessionEntry::ModelChange(ModelChangeEntry {
            provider: ProviderId::new("scripted"),
            model_id: ModelId::new("reasoning"),
        }),
        SessionEntry::ThinkingLevelChange(ThinkingLevelEntry {
            thinking_level: "high".into(),
        }),
        SessionEntry::ActiveToolsChange(ActiveToolsEntry {
            active_tool_names: vec!["echo".into()],
        }),
    ])
    .unwrap();
}

struct Observation {
    snapshot: AgentSessionSnapshot,
    document: Arc<SessionDocument>,
    prompt_options: Option<Value>,
}

impl Observation {
    fn capture(session: &AgentSession) -> Self {
        Self {
            snapshot: session.snapshot(),
            document: session.history().shared_document().unwrap(),
            prompt_options: session.catalog().prompt_options(),
        }
    }

    fn assert_unchanged(&self, session: &AgentSession) {
        let current = session.snapshot();
        assert_eq!(current.revision, self.snapshot.revision);
        assert_eq!(
            current.agent.system_prompt,
            self.snapshot.agent.system_prompt
        );
        assert_eq!(current.agent.provider_id, self.snapshot.agent.provider_id);
        assert_eq!(current.agent.model_id, self.snapshot.agent.model_id);
        assert_eq!(
            current.agent.thinking_level,
            self.snapshot.agent.thinking_level
        );
        assert_eq!(current.agent.active_tools, self.snapshot.agent.active_tools);
        assert!(Arc::ptr_eq(
            &current.agent.messages,
            &self.snapshot.agent.messages
        ));
        assert_eq!(session.catalog().prompt_options(), self.prompt_options);
        assert!(Arc::ptr_eq(
            &session.history().shared_document().unwrap(),
            &self.document,
        ));
    }
}

#[tokio::test]
async fn invalid_selection_or_prompt_render_leaves_configuration_journal_and_events_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = runtime_builder(directory.path(), ScriptedProviderPlugin::scripted([]))
        .build()
        .unwrap();
    let session = AgentSession::create(runtime, directory.path().join("session.jsonl"), options())
        .await
        .unwrap();
    let before = Observation::capture(&session);
    let mut events = session.subscribe().events;

    let error = session
        .set_model(ProviderId::new("missing"), ModelId::new("plain"))
        .unwrap_err();
    assert!(error.to_string().contains("provider not found"));
    before.assert_unchanged(&session);

    let error = session.set_active_tools(["delay", "missing"]).unwrap_err();
    assert!(error.to_string().contains("missing"));
    before.assert_unchanged(&session);

    let error = session.set_active_tools(["delay", "fail"]).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("fixture prompt rendering failed")
    );
    before.assert_unchanged(&session);
    assert!(events.try_recv().is_err());
    assert!(session.is_idle());
    assert!(!session.history().is_materialized());

    session
        .set_active_tools(["delay", "echo", "delay"])
        .unwrap();
    assert_eq!(session.snapshot().agent.active_tools, ["delay", "echo"]);
    assert_eq!(session.snapshot().agent.system_prompt, "delay,echo");
    assert_eq!(
        session.catalog().prompt_options(),
        Some(json!({"tools": ["delay", "echo"]}))
    );
}

#[tokio::test]
async fn journal_open_failure_rolls_back_every_configuration_setter_and_releases_guards() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    saved_log(&path, directory.path(), "configuration-write");
    let runtime = runtime_builder(directory.path(), ScriptedProviderPlugin::scripted([]))
        .build()
        .unwrap();
    let session = AgentSession::open(runtime, &path, options()).await.unwrap();
    let before = Observation::capture(&session);
    let mut events = session.subscribe().events;
    let original_bytes = std::fs::read(&path).unwrap();
    let backup = directory.path().join("session-backup.jsonl");
    std::fs::rename(&path, &backup).unwrap();
    std::fs::create_dir(&path).unwrap();

    assert!(matches!(
        session.set_model(ProviderId::new("scripted"), ModelId::new("plain")),
        Err(SessionError::Io(_))
    ));
    before.assert_unchanged(&session);
    assert!(matches!(
        session.set_active_tools(["delay"]),
        Err(SessionError::Io(_))
    ));
    before.assert_unchanged(&session);
    assert!(matches!(
        session.set_thinking_level(ThinkingLevel::Low),
        Err(SessionError::Io(_))
    ));
    before.assert_unchanged(&session);
    assert!(events.try_recv().is_err());
    assert!(session.is_idle());
    assert_eq!(std::fs::read(&backup).unwrap(), original_bytes);

    std::fs::remove_dir(&path).unwrap();
    std::fs::rename(&backup, &path).unwrap();
    session
        .set_model(ProviderId::new("scripted"), ModelId::new("plain"))
        .unwrap();
    let after = session.snapshot();
    assert_eq!(after.agent.model_id.as_str(), "plain");
    assert_eq!(after.agent.thinking_level, ThinkingLevel::Off);
    assert_eq!(
        session.history().load().unwrap().entries.len(),
        before.document.entries.len() + 2
    );
    assert_eq!(
        SessionStore::read(&path).unwrap().entries.len(),
        before.document.entries.len() + 2
    );
}

#[tokio::test]
async fn model_and_thinking_commit_in_order_with_immediate_snapshot_reload_and_resume() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    saved_log(&path, directory.path(), "configuration-reload");
    let manager = MultiSessionManager::new(|request: SessionGenerationRequest| async move {
        let runtime = runtime_builder(
            request.workspace.cwd(),
            ScriptedProviderPlugin::scripted([]),
        )
        .workspace(request.workspace)
        .build()
        .unwrap();
        Ok(PreparedSessionGeneration::new(runtime, options()))
    });
    let managed = manager
        .open_session(&path, Default::default())
        .await
        .unwrap();
    let session = managed.current();
    let before = Observation::capture(&session);
    let mut events = session.subscribe().events;
    session
        .set_model(ProviderId::new("scripted"), ModelId::new("plain"))
        .unwrap();

    let after = session.snapshot();
    assert_eq!(after.agent.model_id.as_str(), "plain");
    assert_eq!(after.agent.thinking_level, ThinkingLevel::Off);
    let document = session.history().load().unwrap();
    let changes = &document.entries[before.document.entries.len()..];
    assert_eq!(changes.len(), 2);
    assert!(
        matches!(&changes[0].entry, SessionEntry::ModelChange(entry) if entry.model_id.as_str() == "plain")
    );
    assert!(
        matches!(&changes[1].entry, SessionEntry::ThinkingLevelChange(entry) if entry.thinking_level == "off")
    );
    assert!(
        matches!(events.try_recv().unwrap().event, AgentSessionEvent::EntryAppended { entry } if entry.id == changes[0].id)
    );
    assert!(
        matches!(events.try_recv().unwrap().event, AgentSessionEvent::EntryAppended { entry } if entry.id == changes[1].id)
    );
    assert!(matches!(
        events.try_recv().unwrap().event,
        AgentSessionEvent::ThinkingLevelChanged {
            level: ThinkingLevel::Off
        }
    ));
    assert!(events.try_recv().is_err());

    let unchanged = Observation::capture(&session);
    session.set_thinking_level(ThinkingLevel::High).unwrap();
    unchanged.assert_unchanged(&session);
    assert!(events.try_recv().is_err());
    session
        .set_model(ProviderId::new("scripted"), ModelId::new("reasoning"))
        .unwrap();
    assert_eq!(
        session.history().load().unwrap().entries.len(),
        document.entries.len() + 1
    );
    assert!(
        matches!(events.try_recv().unwrap().event, AgentSessionEvent::EntryAppended { entry } if matches!(entry.entry, SessionEntry::ModelChange(_)))
    );
    assert!(events.try_recv().is_err());
    session.set_thinking_level(ThinkingLevel::Low).unwrap();
    session
        .set_active_tools(["delay", "echo", "delay"])
        .unwrap();
    let expected = session.snapshot().agent;

    managed.reload().await.unwrap();
    let reloaded = managed.current();
    assert!(session.is_closed());
    assert_eq!(reloaded.snapshot().agent.model_id, expected.model_id);
    assert_eq!(
        reloaded.snapshot().agent.thinking_level,
        expected.thinking_level
    );
    assert_eq!(
        reloaded.snapshot().agent.active_tools,
        expected.active_tools
    );
    assert_eq!(
        reloaded.snapshot().agent.system_prompt,
        expected.system_prompt
    );
    manager.shutdown().await.unwrap();

    let runtime = runtime_builder(directory.path(), ScriptedProviderPlugin::scripted([]))
        .build()
        .unwrap();
    let resumed = AgentSession::open(runtime, &path, options()).await.unwrap();
    assert_eq!(resumed.snapshot().agent.model_id, expected.model_id);
    assert_eq!(
        resumed.snapshot().agent.thinking_level,
        expected.thinking_level
    );
    assert_eq!(resumed.snapshot().agent.active_tools, expected.active_tools);
    assert_eq!(
        resumed.snapshot().agent.system_prompt,
        expected.system_prompt
    );
}

#[tokio::test]
async fn busy_and_closed_sessions_reject_configuration_without_mutations() {
    let directory = tempfile::tempdir().unwrap();
    let provider = ScriptedProviderPlugin::scripted([ScriptedTurn::WaitForAbort]);
    let requests = provider.provider();
    let runtime = runtime_builder(directory.path(), provider).build().unwrap();
    let session = AgentSession::create(runtime, directory.path().join("session.jsonl"), options())
        .await
        .unwrap();
    let running = session.clone();
    let run = tokio::spawn(async move { running.prompt("wait for cancellation").await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while requests.requests().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    let before = Observation::capture(&session);
    let mut events = session.subscribe().events;
    assert!(matches!(
        session.set_model(ProviderId::new("scripted"), ModelId::new("plain")),
        Err(SessionError::Busy)
    ));
    assert!(matches!(
        session.set_thinking_level(ThinkingLevel::Low),
        Err(SessionError::Busy)
    ));
    assert!(matches!(
        session.set_active_tools(["delay"]),
        Err(SessionError::Busy)
    ));
    before.assert_unchanged(&session);
    assert!(events.try_recv().is_err());

    session.abort();
    tokio::time::timeout(Duration::from_secs(2), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    session.shutdown().await;
    let before = Observation::capture(&session);
    let mut events = session.subscribe().events;
    assert!(matches!(
        session.set_model(ProviderId::new("scripted"), ModelId::new("plain")),
        Err(SessionError::Closed)
    ));
    assert!(matches!(
        session.set_thinking_level(ThinkingLevel::Low),
        Err(SessionError::Closed)
    ));
    assert!(matches!(
        session.set_active_tools(["delay"]),
        Err(SessionError::Closed)
    ));
    before.assert_unchanged(&session);
    assert!(events.try_recv().is_err());
}
