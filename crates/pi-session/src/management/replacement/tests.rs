use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use pi_agent::AgentOptions;
use pi_core::{Message, ModelId, PluginId, ProviderId, UserMessage};
use pi_plugin::{Plugin, RegisterContext};
use pi_runtime::PiRuntime;
use pi_test_support::ScriptedProviderPlugin;

use super::*;
use crate::{
    AgentSessionOptions, MultiSessionManager, MultiSessionManagerError, PluginError,
    SessionPluginContext,
};

#[derive(Clone)]
struct TestFactory {
    events: Arc<StdMutex<Vec<String>>>,
    requests: Arc<StdMutex<Vec<GenerationRequestSnapshot>>>,
    cancel_switch: Arc<AtomicBool>,
    fail_prepare: Arc<AtomicBool>,
    fail_session_plugin_load: Arc<AtomicBool>,
    prepare_count: Arc<AtomicUsize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GenerationRequestSnapshot {
    cwd: PathBuf,
    session_path: PathBuf,
    reason: SessionStartReason,
    reload_model: Option<ModelSelection>,
    has_initial_state: bool,
}

impl TestFactory {
    fn new() -> Self {
        Self {
            events: Arc::new(StdMutex::new(Vec::new())),
            requests: Arc::new(StdMutex::new(Vec::new())),
            cancel_switch: Arc::new(AtomicBool::new(false)),
            fail_prepare: Arc::new(AtomicBool::new(false)),
            fail_session_plugin_load: Arc::new(AtomicBool::new(false)),
            prepare_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn events(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn record(&self, event: impl Into<String>) {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(event.into());
    }

    fn requests(&self) -> Vec<GenerationRequestSnapshot> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

struct LifecyclePlugin {
    events: Arc<StdMutex<Vec<String>>>,
    cancel_switch: Arc<AtomicBool>,
}

struct OverlayPlugin;

struct StagedActivation {
    committed: Arc<AtomicBool>,
    rolled_back: Arc<AtomicBool>,
    armed: bool,
}

impl StagedActivation {
    fn new(committed: Arc<AtomicBool>, rolled_back: Arc<AtomicBool>) -> Self {
        Self {
            committed,
            rolled_back,
            armed: true,
        }
    }

    fn commit(mut self) {
        self.committed.store(true, Ordering::Release);
        self.armed = false;
    }
}

impl Drop for StagedActivation {
    fn drop(&mut self) {
        if self.armed {
            self.rolled_back.store(true, Ordering::Release);
        }
    }
}

struct RecordedActivation {
    factory: TestFactory,
    label: &'static str,
    staged: Option<StagedActivation>,
}

impl SessionGenerationActivation for RecordedActivation {
    fn commit(mut self: Box<Self>) {
        if let Some(staged) = self.staged.take() {
            staged.commit();
        }
        self.factory.record(self.label);
    }
}

#[derive(Clone)]
struct ActivationFactory {
    inner: TestFactory,
    committed: Arc<AtomicBool>,
    rolled_back: Arc<AtomicBool>,
}

#[async_trait]
impl SessionGenerationFactory for ActivationFactory {
    async fn prepare_generation(
        &self,
        request: SessionGenerationRequest,
    ) -> Result<PreparedSessionGeneration, SessionError> {
        let generation = self.inner.prepare_generation(request).await?;
        Ok(generation
            .with_activation(RecordedActivation {
                factory: self.inner.clone(),
                label: "commit:first",
                staged: None,
            })
            .with_activation(RecordedActivation {
                factory: self.inner.clone(),
                label: "commit:second",
                staged: Some(StagedActivation::new(
                    Arc::clone(&self.committed),
                    Arc::clone(&self.rolled_back),
                )),
            }))
    }
}

#[pi_plugin::plugin]
impl Plugin for OverlayPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("session-overlay")
    }

    fn register(&self, _context: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
        Ok(())
    }
}

impl LifecyclePlugin {
    fn record(&self, event: impl Into<String>) {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(event.into());
    }
}

#[pi_plugin::plugin]
impl Plugin for LifecyclePlugin {
    fn id(&self) -> PluginId {
        PluginId::new("runtime-lifecycle")
    }

    async fn session_start(
        &self,
        _context: &SessionPluginContext,
        event: &SessionStartEvent,
    ) -> Result<(), PluginError> {
        self.record(format!("start:{:?}", event.reason));
        Ok(())
    }

    async fn session_before_switch(
        &self,
        _context: &SessionPluginContext,
        event: &SessionBeforeSwitchEvent,
    ) -> Result<Option<crate::SessionBeforeSwitchResult>, PluginError> {
        self.record(format!("before:{:?}", event.reason));
        Ok(Some(crate::SessionBeforeSwitchResult {
            cancel: self.cancel_switch.load(Ordering::Acquire),
        }))
    }

    async fn session_shutdown(
        &self,
        _context: &SessionPluginContext,
        event: &SessionShutdownEvent,
    ) -> Result<(), PluginError> {
        self.record(format!("shutdown:{:?}", event.reason));
        Ok(())
    }
}

#[async_trait]
impl SessionGenerationFactory for TestFactory {
    async fn prepare_generation(
        &self,
        request: SessionGenerationRequest,
    ) -> Result<PreparedSessionGeneration, SessionError> {
        self.prepare_count.fetch_add(1, Ordering::AcqRel);
        self.record(format!("prepare:{:?}", request.reason));
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(GenerationRequestSnapshot {
                cwd: request.cwd.clone(),
                session_path: request.session_path.clone(),
                reason: request.reason,
                reload_model: request.reload_model.clone(),
                has_initial_state: request.initial_state.is_some(),
            });
        if self.fail_prepare.load(Ordering::Acquire) {
            return Err(SessionError::Runtime(
                "fixture preparation failed".to_string(),
            ));
        }

        let generation_overlay = request.generation_overlay;
        let mut builder = PiRuntime::builder()
            .provider_plugin(ScriptedProviderPlugin::scripted([]))
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("scripted"),
                model_id: ModelId::new("test"),
                cwd: request.cwd,
                ..AgentOptions::default()
            });
        builder = generation_overlay.apply_to(builder);
        let plugin_events = Arc::clone(&self.events);
        let cancel_switch = Arc::clone(&self.cancel_switch);
        let fail_session_plugin_load = Arc::clone(&self.fail_session_plugin_load);
        let runtime = builder
            .try_plugin_arc_factory(move || {
                if fail_session_plugin_load.load(Ordering::Acquire) {
                    return Err("fixture session plugin load failed");
                }
                Ok(Arc::new(LifecyclePlugin {
                    events: Arc::clone(&plugin_events),
                    cancel_switch: Arc::clone(&cancel_switch),
                }) as Arc<dyn Plugin>)
            })
            .build()?;
        Ok(PreparedSessionGeneration::new(
            runtime,
            AgentSessionOptions::default(),
        ))
    }
}

#[tokio::test]
async fn factory_receives_resolved_generation_context_without_owning_session_storage() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path().join("project");
    let path = directory.path().join("session.jsonl");
    std::fs::create_dir_all(&cwd).unwrap();
    let factory = TestFactory::new();
    let manager = MultiSessionManager::new(factory.clone());
    let session = manager
        .create_session(pi_session::CreateSessionOptions::new(&cwd, &path))
        .await
        .unwrap();

    assert_eq!(session.path(), path);
    assert_eq!(
        factory.requests(),
        [GenerationRequestSnapshot {
            cwd: cwd.clone(),
            session_path: path.clone(),
            reason: SessionStartReason::Startup,
            reload_model: None,
            has_initial_state: false,
        }]
    );

    session.reload().await.unwrap();

    assert_eq!(session.path(), path);
    assert_eq!(
        factory.requests()[1],
        GenerationRequestSnapshot {
            cwd,
            session_path: path,
            reason: SessionStartReason::Reload,
            reload_model: Some(ModelSelection::new("scripted", "test")),
            has_initial_state: false,
        }
    );
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn prepared_session_defers_external_state_until_activation() {
    let directory = tempfile::tempdir().unwrap();
    let inner = TestFactory::new();
    let committed = Arc::new(AtomicBool::new(false));
    let rolled_back = Arc::new(AtomicBool::new(false));
    let factory = ActivationFactory {
        inner: inner.clone(),
        committed: Arc::clone(&committed),
        rolled_back: Arc::clone(&rolled_back),
    };
    let start_event = SessionStartEvent {
        reason: SessionStartReason::Startup,
        previous_session_file: None,
    };
    let prepared = SessionSlot::prepare_session(
        &factory,
        SessionTarget::create(directory.path(), directory.path().join("session.jsonl")),
        start_event.reason,
        SessionGenerationOverlay::default(),
        None,
        None,
    )
    .await
    .unwrap();

    assert!(!committed.load(Ordering::Acquire));
    assert!(!rolled_back.load(Ordering::Acquire));
    assert_eq!(inner.events(), ["prepare:Startup"]);

    let session = prepared.activate(start_event).await;

    assert!(committed.load(Ordering::Acquire));
    assert!(!rolled_back.load(Ordering::Acquire));
    assert_eq!(
        inner.events(),
        [
            "prepare:Startup",
            "commit:first",
            "commit:second",
            "start:Startup",
        ]
    );
    session.shutdown().await;
}

#[tokio::test]
async fn dropping_a_prepared_session_rolls_back_staged_external_state() {
    let directory = tempfile::tempdir().unwrap();
    let inner = TestFactory::new();
    let committed = Arc::new(AtomicBool::new(false));
    let rolled_back = Arc::new(AtomicBool::new(false));
    let factory = ActivationFactory {
        inner: inner.clone(),
        committed: Arc::clone(&committed),
        rolled_back: Arc::clone(&rolled_back),
    };
    let prepared = SessionSlot::prepare_session(
        &factory,
        SessionTarget::create(directory.path(), directory.path().join("session.jsonl")),
        SessionStartReason::Startup,
        SessionGenerationOverlay::default(),
        None,
        None,
    )
    .await
    .unwrap();

    drop(prepared);

    assert!(!committed.load(Ordering::Acquire));
    assert!(rolled_back.load(Ordering::Acquire));
    assert_eq!(inner.events(), ["prepare:Startup"]);
}

#[tokio::test]
async fn transient_generation_overlay_is_rebuilt_across_reload_and_replacement() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("first.jsonl");
    let loads = Arc::new(AtomicUsize::new(0));
    let factory_loads = Arc::clone(&loads);
    let overlay = SessionGenerationOverlay::new().with_plugin(move || {
        factory_loads.fetch_add(1, Ordering::AcqRel);
        Arc::new(OverlayPlugin)
    });
    let manager = MultiSessionManager::new(TestFactory::new());
    let session = manager
        .create_session(pi_session::CreateSessionOptions {
            generation_overlay: overlay,
            ..pi_session::CreateSessionOptions::new(directory.path(), &path)
        })
        .await
        .unwrap();

    assert!(
        session
            .current()
            .runtime()
            .plugin_order()
            .contains(&PluginId::new("session-overlay"))
    );
    assert_eq!(loads.load(Ordering::Acquire), 1);

    session.reload().await.unwrap();
    assert!(
        session
            .current()
            .runtime()
            .plugin_order()
            .contains(&PluginId::new("session-overlay"))
    );
    assert_eq!(loads.load(Ordering::Acquire), 2);

    session
        .new_session(directory.path(), directory.path().join("second.jsonl"))
        .await
        .unwrap();
    assert!(
        session
            .current()
            .runtime()
            .plugin_order()
            .contains(&PluginId::new("session-overlay"))
    );
    assert_eq!(loads.load(Ordering::Acquire), 3);
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn new_and_resume_publish_only_after_ordered_lifecycle_transition() {
    let directory = tempfile::tempdir().unwrap();
    let first_path = directory.path().join("first.jsonl");
    let second_path = directory.path().join("second.jsonl");
    let factory = TestFactory::new();
    let manager = MultiSessionManager::new(factory.clone());
    let session = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            &first_path,
        ))
        .await
        .unwrap();
    let mut changes = session.subscribe();
    let first = session.current();
    first.store().materialize().unwrap();

    let outcome = session
        .new_session(directory.path(), &second_path)
        .await
        .unwrap();

    assert_eq!(outcome, AgentSessionReplacement::Replaced);
    changes.changed().await.unwrap();
    let second = session.current();
    assert!(!Arc::ptr_eq(&first, &second));
    assert!(first.is_closed());
    assert!(matches!(
        first.set_name(Some("stale".to_string())).await,
        Err(SessionError::Closed)
    ));
    assert_eq!(second.store().path(), second_path);
    assert_eq!(
        factory.events(),
        vec![
            "prepare:Startup",
            "start:Startup",
            "before:New",
            "prepare:New",
            "shutdown:New",
            "start:New",
        ]
    );

    let outcome = session.resume_session(&first_path).await.unwrap();

    assert_eq!(outcome, AgentSessionReplacement::Replaced);
    assert_eq!(session.path(), first_path);
    assert_eq!(
        &factory.events()[6..],
        [
            "before:Resume",
            "prepare:Resume",
            "shutdown:Resume",
            "start:Resume",
        ]
    );
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn new_session_resolves_an_unsaved_parent_path_to_its_id() {
    let directory = tempfile::tempdir().unwrap();
    let parent_path = directory.path().join("parent.jsonl");
    let child_path = directory.path().join("child.jsonl");
    let manager = MultiSessionManager::new(TestFactory::new());
    let session = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            &parent_path,
        ))
        .await
        .unwrap();
    let parent_id = session.id();

    session
        .new_session_with_parent(directory.path(), &child_path, &parent_path)
        .await
        .unwrap();
    session.reload().await.unwrap();

    assert_eq!(
        session.current().store().header().parent_session_id,
        Some(parent_id.clone())
    );
    assert!(!parent_path.exists());
    assert!(!child_path.exists());
    session.current().store().materialize().unwrap();
    assert_eq!(
        SessionStore::read(&child_path)
            .unwrap()
            .header
            .parent_session_id,
        Some(parent_id)
    );
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn new_session_reads_a_saved_parent_id_without_rewriting_its_file() {
    let directory = tempfile::tempdir().unwrap();
    let parent_path = directory.path().join("parent.jsonl");
    let header = SessionHeader::new("saved-parent-id", directory.path());
    let original = serde_json::to_string(&header).unwrap();
    std::fs::write(&parent_path, &original).unwrap();
    let manager = MultiSessionManager::new(TestFactory::new());
    let session = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            directory.path().join("initial.jsonl"),
        ))
        .await
        .unwrap();

    session
        .new_session_with_parent(
            directory.path(),
            directory.path().join("child.jsonl"),
            &parent_path,
        )
        .await
        .unwrap();

    assert_eq!(
        session
            .current()
            .store()
            .header()
            .parent_session_id
            .as_deref(),
        Some("saved-parent-id")
    );
    assert_eq!(std::fs::read_to_string(&parent_path).unwrap(), original);
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn new_session_rejects_an_unknown_parent_without_replacing_the_current_session() {
    let directory = tempfile::tempdir().unwrap();
    let child_path = directory.path().join("child.jsonl");
    let factory = TestFactory::new();
    let manager = MultiSessionManager::new(factory.clone());
    let session = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            directory.path().join("initial.jsonl"),
        ))
        .await
        .unwrap();
    let current = session.current();

    assert!(
        session
            .new_session_with_parent(
                directory.path(),
                &child_path,
                directory.path().join("missing.jsonl"),
            )
            .await
            .is_err()
    );

    assert!(Arc::ptr_eq(&current, &session.current()));
    assert!(!current.is_closed());
    assert!(!child_path.exists());
    assert_eq!(factory.prepare_count.load(Ordering::Acquire), 1);
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn fork_copies_the_selected_branch_and_replaces_with_fork_lifecycle() {
    let directory = tempfile::tempdir().unwrap();
    let first_path = directory.path().join("first.jsonl");
    let factory = TestFactory::new();
    let manager = MultiSessionManager::new(factory.clone());
    let session = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            &first_path,
        ))
        .await
        .unwrap();
    let first = session.current();
    let user = first
        .store()
        .append_message(Message::User(UserMessage::text("fork here", 1)))
        .unwrap();
    first.store().materialize().unwrap();

    let outcome = session
        .fork_session(&user, ForkPosition::Before)
        .await
        .unwrap();

    assert_eq!(outcome, AgentSessionReplacement::Replaced);
    assert!(first.is_closed());
    let fork = session.current();
    assert_ne!(fork.store().path(), first_path);
    assert_eq!(
        fork.store().header().parent_session_id.as_deref(),
        Some(first.store().header().id.as_str())
    );
    assert!(fork.store().get_entry(&user).is_none());
    assert_eq!(
        &factory.events()[2..],
        ["prepare:Fork", "shutdown:Fork", "start:Fork"]
    );
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancelled_switch_does_not_prepare_or_replace() {
    let directory = tempfile::tempdir().unwrap();
    let first_path = directory.path().join("first.jsonl");
    let factory = TestFactory::new();
    let manager = MultiSessionManager::new(factory.clone());
    let session = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            &first_path,
        ))
        .await
        .unwrap();
    let first = session.current();
    factory.cancel_switch.store(true, Ordering::Release);

    let outcome = session
        .new_session(directory.path(), directory.path().join("second.jsonl"))
        .await
        .unwrap();

    assert_eq!(outcome, AgentSessionReplacement::Cancelled);
    assert!(Arc::ptr_eq(&first, &session.current()));
    assert_eq!(factory.prepare_count.load(Ordering::Acquire), 1);
    assert_eq!(
        factory.events(),
        vec!["prepare:Startup", "start:Startup", "before:New"]
    );
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn preparation_failure_keeps_current_session_active() {
    let directory = tempfile::tempdir().unwrap();
    let first_path = directory.path().join("first.jsonl");
    let factory = TestFactory::new();
    let manager = MultiSessionManager::new(factory.clone());
    let session = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            &first_path,
        ))
        .await
        .unwrap();
    let first = session.current();
    factory.fail_prepare.store(true, Ordering::Release);

    let error = session
        .new_session(directory.path(), directory.path().join("second.jsonl"))
        .await
        .unwrap_err();

    assert!(matches!(error, MultiSessionManagerError::Session(_)));
    assert!(Arc::ptr_eq(&first, &session.current()));
    assert!(!first.is_closed());
    first
        .set_name(Some("still active".to_string()))
        .await
        .unwrap();
    assert_eq!(
        factory.events(),
        vec![
            "prepare:Startup",
            "start:Startup",
            "before:New",
            "prepare:New",
        ]
    );
    factory.fail_prepare.store(false, Ordering::Release);
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn session_plugin_load_failure_keeps_whole_generation_active() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    let factory = TestFactory::new();
    let manager = MultiSessionManager::new(factory.clone());
    let session = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            &path,
        ))
        .await
        .unwrap();
    let current = session.current();
    factory
        .fail_session_plugin_load
        .store(true, Ordering::Release);

    let error = session.reload().await.unwrap_err();

    assert!(
        error
            .to_string()
            .contains("fixture session plugin load failed")
    );
    assert!(Arc::ptr_eq(&current, &session.current()));
    assert!(!current.is_closed());
    assert_eq!(
        factory.events(),
        ["prepare:Startup", "start:Startup", "prepare:Reload"]
    );
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn reload_rebuilds_the_whole_session_and_manager_shutdown_is_idempotent() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    let factory = TestFactory::new();
    let manager = MultiSessionManager::new(factory.clone());
    let session = manager
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            &path,
        ))
        .await
        .unwrap();
    let first = session.current();
    assert!(!path.exists());

    session.reload().await.unwrap();

    assert!(!Arc::ptr_eq(&first, &session.current()));
    assert_eq!(session.path(), path);
    assert!(!path.exists());
    assert!(!session.current().store().is_materialized());
    assert_eq!(
        factory.events(),
        vec![
            "prepare:Startup",
            "start:Startup",
            "prepare:Reload",
            "shutdown:Reload",
            "start:Reload",
        ]
    );

    manager.shutdown().await.unwrap();
    manager.shutdown().await.unwrap();
    assert_eq!(
        factory.events().last().map(String::as_str),
        Some("shutdown:Quit")
    );
    assert!(matches!(
        session.reload().await.unwrap_err(),
        MultiSessionManagerError::Closed
    ));
}
