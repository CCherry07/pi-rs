//! Settings -> Coding options -> public AgentSession compaction conformance.
//! Oracle: current legacy settings-manager-compaction and model-overrides suite.

use std::path::Path;
use std::sync::{Arc, Mutex};

use pi_agent::AgentOptions;
use pi_core::{
    AssistantMessage, Message, ModelId, ModelSpec, PluginId, ProviderId, ResponseMetadata,
    StopReason, StreamEvent, Usage, UserMessage,
};
use pi_plugin::{
    Plugin, ProviderPlugin, ProviderRegisterContext, SessionBeforeCompactEvent,
    SessionBeforeCompactResult, SessionPluginContext,
};
use pi_runtime::PiRuntime;
use pi_session::{
    AgentSession, CompactionEntry, CompactionReason, ModelChangeEntry, SessionEntry, SessionHeader,
    SessionStore,
};
use pi_settings::{SettingsContext, SettingsManager};
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};
use serde_json::{Value, json};

use super::{AgentSessionOptions, InitialModelRequest, session_options};

type Preparations = Arc<Mutex<Vec<SessionBeforeCompactEvent>>>;

struct Capture(Preparations);

#[pi_plugin::plugin]
impl Plugin for Capture {
    fn id(&self) -> PluginId {
        "compaction-settings-capture".into()
    }

    async fn session_before_compact(
        &self,
        _context: &SessionPluginContext,
        event: &SessionBeforeCompactEvent,
    ) -> Result<Option<SessionBeforeCompactResult>, pi_plugin::PluginError> {
        self.0.lock().unwrap().push(event.clone());
        Ok(Some(SessionBeforeCompactResult {
            cancel: false,
            compaction: Some(CompactionEntry {
                first_kept_entry_id: None,
                summary: "resolved model summary".into(),
                retained_tail: event.preparation.retained_tail.clone(),
                retained_entry_ids: Vec::new(),
                tokens_before: event.preparation.tokens_before,
                details: None,
                usage: None,
                from_hook: None,
                system_message: None,
            }),
        }))
    }
}

struct Catalog;

#[pi_plugin::provider_plugin]
impl ProviderPlugin for Catalog {
    fn id(&self) -> PluginId {
        "compaction-settings-catalog".into()
    }

    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_plugin::Result<()> {
        for (id, window) in [("small", 4000), ("family/big", 10000)] {
            let mut model = ModelSpec::new("scripted", id, id, "scripted");
            model.context_window = window;
            context.register_model(model)?;
        }
        Ok(())
    }
}

fn runtime(cwd: &Path, turns: Vec<ScriptedTurn>, preparations: Preparations) -> PiRuntime {
    PiRuntime::builder()
        .provider_plugin(ScriptedProviderPlugin::scripted(turns))
        .provider_plugin(Catalog)
        .plugin(Capture(preparations))
        .agent_options(AgentOptions {
            provider_id: "scripted".into(),
            model_id: "small".into(),
            cwd: cwd.to_path_buf(),
            ..AgentOptions::default()
        })
        .build()
        .unwrap()
}

fn options(root: &Path, compaction: Value) -> AgentSessionOptions {
    std::fs::write(
        root.join("settings.json"),
        json!({"compaction":compaction}).to_string(),
    )
    .unwrap();
    let snapshot = SettingsManager::new(root).load(&SettingsContext::new(root, false));
    session_options(snapshot.effective(), InitialModelRequest::default())
}

fn seed(path: &Path, total_tokens: u64, model: &str) {
    let store = SessionStore::create(
        path,
        SessionHeader::new("compaction-settings", path.parent().unwrap()),
    )
    .unwrap();
    store
        .append(SessionEntry::ModelChange(ModelChangeEntry {
            provider: "scripted".into(),
            model_id: model.into(),
        }))
        .unwrap();
    for label in ["old", "recent"] {
        store
            .append_message(Message::User(UserMessage::text(
                format!("{label}{}", "x".repeat(400)),
                1,
            )))
            .unwrap();
        let assistant: AssistantMessage = serde_json::from_value(json!({
            "content": [{"type":"text", "text":format!("{label}{}", "y".repeat(400))}],
            "api":"scripted", "provider":"scripted", "model":model,
            "usage":Usage { input: total_tokens, total_tokens, ..Usage::default() },
            "stopReason":"stop", "timestamp":2
        }))
        .unwrap();
        store
            .append_message(Message::Assistant(assistant.into()))
            .unwrap();
    }
}

fn answer(text: &str, tokens: u64) -> ScriptedTurn {
    ScriptedTurn::Events(vec![
        StreamEvent::Start {
            metadata: ResponseMetadata::new("scripted".into(), "small".into(), "scripted", 3),
        },
        StreamEvent::TextStart { content_index: 0 },
        StreamEvent::TextDelta {
            content_index: 0,
            delta: text.into(),
        },
        StreamEvent::TextEnd {
            content_index: 0,
            text_signature: None,
        },
        StreamEvent::Done {
            reason: StopReason::Stop,
            usage: Usage {
                input: tokens,
                total_tokens: tokens,
                ..Usage::default()
            },
        },
    ])
}

#[tokio::test]
async fn model_overrides_reach_manual_pre_prompt_post_run_and_overflow_preparation() {
    for path_kind in ["manual", "pre-prompt", "post-run", "overflow"] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("session.jsonl");
        seed(
            &path,
            if path_kind == "pre-prompt" { 2500 } else { 650 },
            "small",
        );
        let preparations: Preparations = Arc::default();
        let turns = match path_kind {
            "manual" => vec![],
            "overflow" => vec![
                ScriptedTurn::Error("prompt is too long".into()),
                answer("recovered", 10),
            ],
            "post-run" => vec![answer(&"z".repeat(8000), 2500)],
            _ => vec![answer("done", 10)],
        };
        let session = AgentSession::open(
            runtime(root.path(), turns, preparations.clone()), &path,
            options(root.path(), json!({
                "enabled": path_kind != "manual", "reserveTokens":10, "keepRecentTokens":20000,
                "modelOverrides":{"scripted/small":{"reserveTokens":2000,"keepRecentTokens":150}}
            })),
        ).await.unwrap();
        if path_kind == "manual" {
            session.compact(None).await.unwrap();
        } else {
            session.prompt("continue").await.unwrap();
        }
        let events = preparations.lock().unwrap().clone();
        assert_eq!(events.len(), 1, "{path_kind}");
        assert_eq!(
            events[0].preparation.settings,
            pi_session::CompactionSettings {
                enabled: path_kind != "manual",
                reserve_tokens: 2000,
                keep_recent_tokens: 150,
            },
            "{path_kind}"
        );
        assert_eq!(
            events[0].reason,
            match path_kind {
                "manual" => CompactionReason::Manual,
                "overflow" => CompactionReason::Overflow,
                _ => CompactionReason::Threshold,
            }
        );
        if matches!(path_kind, "manual" | "pre-prompt") {
            let first = events[0].preparation.retained_tail.first().unwrap();
            assert!(matches!(first.as_standard(), Some(Message::User(user))
                if matches!(user.content.first(), Some(pi_core::ContentBlock::Text(text)) if text.text.starts_with("recent"))));
        }
        session.shutdown().await;
    }
}

#[tokio::test]
async fn selected_model_changes_budget_and_policy_without_mutating_ordinary_settings() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("session.jsonl");
    seed(&path, 650, "small");
    let preparations: Preparations = Arc::default();
    let options = options(
        root.path(),
        json!({"reserveTokens":10, "keepRecentTokens":20000,
        "modelOverrides":{"scripted/family/big":{"reserveTokens":8000,"keepRecentTokens":150}}}),
    );
    let session = AgentSession::open(
        runtime(
            root.path(),
            vec![
                answer("small response", 2500),
                answer("big response", 10),
                answer("back on small", 10),
            ],
            preparations.clone(),
        ),
        &path,
        options.clone(),
    )
    .await
    .unwrap();
    session.prompt("continue on small").await.unwrap();
    assert!(preparations.lock().unwrap().is_empty());
    assert_eq!(session.context_budget().unwrap().reserve_tokens, 10);
    session
        .set_model(ProviderId::new("scripted"), ModelId::new("family/big"))
        .unwrap();
    // Model changes invalidate the last request snapshot, not its settings.
    assert!(session.context_budget().is_none());
    session.prompt("continue on big").await.unwrap();
    let budget = session.context_budget().unwrap();
    assert_eq!(budget.context_window, Some(10000));
    assert_eq!(budget.reserve_tokens, 8000);
    assert_eq!(preparations.lock().unwrap().len(), 1);
    assert_eq!(
        preparations.lock().unwrap()[0]
            .preparation
            .settings
            .keep_recent_tokens,
        150
    );
    session
        .set_model("scripted".into(), "small".into())
        .unwrap();
    assert!(session.context_budget().is_none());
    session.prompt("continue back on small").await.unwrap();
    assert_eq!(session.context_budget().unwrap().reserve_tokens, 10);
    assert_eq!(options.compaction.reserve_tokens, 10);
    session.shutdown().await;
}

#[tokio::test]
async fn resumed_model_resolves_its_override_not_the_runtime_bootstrap_model() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("session.jsonl");
    seed(&path, 2500, "family/big");
    let preparations: Preparations = Arc::default();
    let session = AgentSession::open(runtime(root.path(), vec![], preparations.clone()), &path,
        options(root.path(), json!({"enabled":false, "reserveTokens":10,
            "modelOverrides":{"scripted/family/big":{"reserveTokens":8000,"keepRecentTokens":150}}})),
    ).await.unwrap();
    assert_eq!(session.snapshot().agent.model_id.as_str(), "family/big");
    session.compact(None).await.unwrap();
    let events = preparations.lock().unwrap().clone();
    assert_eq!(events[0].preparation.settings.reserve_tokens, 8000);
    assert_eq!(events[0].preparation.settings.keep_recent_tokens, 150);
    session.shutdown().await;
}

#[tokio::test]
async fn invalid_selected_model_settings_fail_lazily_and_toggles_still_work() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("session.jsonl");
    seed(&path, 650, "small");
    let preparations: Preparations = Arc::default();
    let session = AgentSession::open(
        runtime(root.path(), vec![], preparations.clone()),
        &path,
        options(
            root.path(),
            json!({"keepRecentTokens":150,
            "modelOverrides":{"scripted/family/big":{"reserveTokens":-1}}}),
        ),
    )
    .await
    .unwrap();
    session.set_auto_compaction_enabled(false);
    session.compact(None).await.unwrap();
    session
        .set_model("scripted".into(), "family/big".into())
        .unwrap();
    session.set_auto_compaction_enabled(true);
    assert!(session.auto_compaction_enabled());
    let error = session.compact(None).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("compaction.modelOverrides[\"scripted/family/big\"].reserveTokens")
    );
    assert_eq!(preparations.lock().unwrap().len(), 1);
    session.shutdown().await;
}
