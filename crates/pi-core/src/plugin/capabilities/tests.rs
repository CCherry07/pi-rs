//! Interface tests for generation-bound plugin capabilities.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value;

use super::*;
use crate::{
    AbortHandle, AssistantMessage, ContentBlock, CustomMessageContent, Message, ModelId,
    ProviderId, StopReason, TextContent, Usage, UserMessage,
};

struct StaticAccess {
    notices: Mutex<Vec<(NoticeLevel, String)>>,
    confirmations: Mutex<Vec<(String, String)>>,
}

#[async_trait]
impl UiContextAccess for StaticAccess {
    fn mode(&self) -> PluginContextResult<PresentationMode> {
        Ok(PresentationMode::Tui)
    }

    fn ui_notify(&self, level: NoticeLevel, message: String) -> PluginContextResult<()> {
        self.notices.lock().unwrap().push((level, message));
        Ok(())
    }

    async fn ui_confirm(&self, title: String, message: String) -> PluginContextResult<bool> {
        self.confirmations.lock().unwrap().push((title, message));
        Ok(true)
    }
}

impl ModelsContextAccess for StaticAccess {}

#[async_trait]
impl SessionContextAccess for StaticAccess {
    fn session_snapshot(&self) -> PluginContextResult<SessionSnapshot> {
        let entry = SessionEntryView::new(
            "entry-1".to_string(),
            None,
            42,
            SessionEntryKind::Custom,
            serde_json::json!({"id": "entry-1", "type": "custom", "future": true}),
        );
        Ok(SessionSnapshot::new(
            "session-1".to_string(),
            PathBuf::from("/workspace"),
            PathBuf::from("/sessions"),
            Some(PathBuf::from("/sessions/session-1.jsonl")),
            Some("demo".to_string()),
            Some("entry-1".to_string()),
            vec![entry.clone()],
            vec![entry],
            BTreeMap::from([("entry-1".to_string(), "checkpoint".to_string())]),
            serde_json::json!({"id": "session-1", "future": true}),
        ))
    }

    fn cwd(&self) -> PluginContextResult<PathBuf> {
        Ok(PathBuf::from("/workspace"))
    }

    fn session_cwd(&self) -> PluginContextResult<PathBuf> {
        Ok(PathBuf::from("/workspace"))
    }

    fn is_project_trusted(&self) -> PluginContextResult<bool> {
        Ok(true)
    }

    fn session_name(&self) -> PluginContextResult<Option<String>> {
        Ok(Some("demo".to_string()))
    }

    fn session_entries(&self) -> PluginContextResult<Vec<Value>> {
        Ok(vec![serde_json::json!({ "id": "entry-1" })])
    }

    async fn complete(
        &self,
        _scope: PluginContextScope,
        request: DirectCompletionRequest,
        signal: crate::AbortSignal,
    ) -> PluginContextResult<AssistantMessage> {
        assert_eq!(request.system_prompt, "review");
        assert_eq!(request.messages.len(), 1);
        assert!(!signal.is_aborted());
        Ok(AssistantMessage {
            content: vec![ContentBlock::Text(TextContent::new("{\"operations\":[]}"))],
            api: "test".to_string(),
            provider: ProviderId::new("test"),
            model: ModelId::new("test"),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            deferred: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp_ms: 0,
        })
    }

    async fn launch_isolated_session(
        &self,
        _scope: PluginContextScope,
        _request: IsolatedSessionRequest,
    ) -> PluginContextResult<IsolatedSessionId> {
        Ok(IsolatedSessionId::new("isolated-1"))
    }

    async fn wait_for_isolated_session(
        &self,
        _scope: PluginContextScope,
        id: IsolatedSessionId,
    ) -> PluginContextResult<IsolatedSessionOutcome> {
        assert_eq!(id.as_str(), "isolated-1");
        Ok(IsolatedSessionOutcome {
            session_id: "child-session".to_string(),
            messages: Vec::new(),
            aborted: false,
        })
    }

    fn abort_isolated_session(
        &self,
        _scope: PluginContextScope,
        id: IsolatedSessionId,
    ) -> PluginContextResult<()> {
        assert_eq!(id.as_str(), "isolated-1");
        Ok(())
    }

    async fn new_session(
        &self,
        scope: PluginContextScope,
        _options: NewSessionOptions,
    ) -> PluginContextResult<PluginContextReplacement> {
        let access: Arc<dyn PluginContext> = Arc::new(StaticAccess {
            notices: Mutex::new(Vec::new()),
            confirmations: Mutex::new(Vec::new()),
        });
        Ok(PluginContextReplacement {
            cancelled: false,
            context: Some(PluginContextEpoch::new(access).handle(scope)),
        })
    }
}

fn access() -> Arc<StaticAccess> {
    Arc::new(StaticAccess {
        notices: Mutex::new(Vec::new()),
        confirmations: Mutex::new(Vec::new()),
    })
}

struct ReplacingAccess {
    old: Mutex<Option<PluginContextEpoch>>,
    next: PluginContextEpoch,
}

#[async_trait]
impl UiContextAccess for ReplacingAccess {}
impl ModelsContextAccess for ReplacingAccess {}

#[async_trait]
impl SessionContextAccess for ReplacingAccess {
    async fn new_session(
        &self,
        scope: PluginContextScope,
        _options: NewSessionOptions,
    ) -> PluginContextResult<PluginContextReplacement> {
        self.old.lock().unwrap().take().unwrap().retire();
        Ok(PluginContextReplacement {
            cancelled: false,
            context: Some(self.next.handle(scope)),
        })
    }

    async fn reload(
        &self,
        scope: PluginContextScope,
    ) -> PluginContextResult<PluginContextReplacement> {
        self.old.lock().unwrap().take().unwrap().retire();
        Ok(PluginContextReplacement {
            cancelled: false,
            context: Some(self.next.handle(scope)),
        })
    }
}

#[tokio::test]
async fn typed_context_calls_the_plugin_contract_without_json_round_trips() {
    let access = access();
    let trait_access: Arc<dyn PluginContext> = access.clone();
    let epoch = PluginContextEpoch::new(trait_access);
    let context = epoch.context();

    assert_eq!(context.ui.mode().unwrap(), PresentationMode::Tui);
    assert_eq!(context.session.cwd().unwrap(), PathBuf::from("/workspace"));
    assert!(context.session.is_project_trusted().unwrap());
    assert_eq!(context.session.name().unwrap().as_deref(), Some("demo"));
    assert_eq!(context.session.entries().unwrap().len(), 1);
    let snapshot = context.session.snapshot().unwrap();
    assert_eq!(snapshot.name(), Some("demo"));
    assert_eq!(snapshot.leaf().unwrap().id(), "entry-1");
    assert_eq!(snapshot.leaf().unwrap().kind(), &SessionEntryKind::Custom);
    assert_eq!(snapshot.label("entry-1"), Some("checkpoint"));
    assert_eq!(snapshot.raw_header()["future"], true);
    assert_eq!(snapshot.leaf().unwrap().raw()["future"], true);

    let (_, signal) = AbortHandle::new();
    let direct = context
        .session
        .complete(
            DirectCompletionRequest {
                system_prompt: "review".to_string(),
                messages: vec![Message::User(UserMessage::text("history", 1))],
                model: None,
                thinking_level: None,
                max_output_tokens: Some(256),
            },
            signal,
        )
        .await
        .unwrap();
    assert!(matches!(
        direct.content.first(),
        Some(ContentBlock::Text(text)) if text.text == "{\"operations\":[]}"
    ));

    let isolated = context
        .session
        .launch_isolated_session(IsolatedSessionRequest::new(CustomMessageContent::Text(
            "inspect".to_string(),
        )))
        .await
        .unwrap();
    assert_eq!(isolated.id().as_str(), "isolated-1");
    isolated.abort().unwrap();
    assert_eq!(isolated.wait().await.unwrap().session_id, "child-session");

    context.ui.notify(NoticeLevel::Info, "ready").unwrap();
    assert_eq!(
        access.notices.lock().unwrap().as_slice(),
        &[(NoticeLevel::Info, "ready".to_string())]
    );
    assert!(
        context
            .ui
            .confirm("Replace session?", "The active session will change.")
            .await
            .unwrap()
    );
    assert_eq!(
        access.confirmations.lock().unwrap().as_slice(),
        &[(
            "Replace session?".to_string(),
            "The active session will change.".to_string()
        )]
    );

    let SessionReplacement::Replaced(replacement) = epoch
        .command_context()
        .session
        .create(NewSessionOptions::default())
        .await
        .unwrap()
    else {
        panic!("expected a replacement context");
    };
    assert_eq!(replacement.ui.mode().unwrap(), PresentationMode::Tui);
}

#[tokio::test]
async fn replacement_handoff_survives_old_epoch_retirement() {
    let next_access: Arc<dyn PluginContext> = access();
    let next = PluginContextEpoch::new(next_access);
    let access = Arc::new(ReplacingAccess {
        old: Mutex::new(None),
        next,
    });
    let old_access: Arc<dyn PluginContext> = access.clone();
    let old = PluginContextEpoch::new(old_access);
    *access.old.lock().unwrap() = Some(old.clone());
    let stale = old.context();

    let SessionReplacement::Replaced(replacement) = old
        .command_context()
        .session
        .create(NewSessionOptions::default())
        .await
        .unwrap()
    else {
        panic!("expected a replacement context");
    };

    assert!(matches!(stale.ui.mode(), Err(PluginContextError::Retired)));
    assert_eq!(replacement.ui.mode().unwrap(), PresentationMode::Tui);
}

#[tokio::test]
async fn reload_handoff_survives_old_epoch_retirement() {
    let next_access: Arc<dyn PluginContext> = access();
    let next = PluginContextEpoch::new(next_access);
    let access = Arc::new(ReplacingAccess {
        old: Mutex::new(None),
        next,
    });
    let old_access: Arc<dyn PluginContext> = access.clone();
    let old = PluginContextEpoch::new(old_access);
    *access.old.lock().unwrap() = Some(old.clone());
    let stale = old.context();

    let replacement = old.command_context().session.reload().await.unwrap();

    assert!(matches!(stale.ui.mode(), Err(PluginContextError::Retired)));
    assert_eq!(replacement.ui.mode().unwrap(), PresentationMode::Tui);
}

#[test]
fn retired_epoch_rejects_stored_native_contexts() {
    let access: Arc<dyn PluginContext> = access();
    let epoch = PluginContextEpoch::new(access);
    let context = epoch.context();
    assert_eq!(context.ui.mode().unwrap(), PresentationMode::Tui);

    epoch.retire();
    assert!(matches!(
        context.ui.mode(),
        Err(PluginContextError::Retired)
    ));
}
