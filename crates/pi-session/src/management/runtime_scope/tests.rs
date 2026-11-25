//! Live ownership identity is independent of persisted paths and executable generations.
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_agent::AgentOptions;
use pi_core::{Message, ModelId, ProviderId, UserMessage};
use pi_runtime::PiRuntime;
use pi_session::{
    AgentSessionOptions, CreateSessionOptions, ForkPosition, LiveRuntimeScopeId,
    MultiSessionManager, PreparedSessionGeneration, SessionError, SessionGenerationFactory,
    SessionGenerationRequest, SessionHeader, SessionStartReason, SessionStore,
};
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};
use tokio::sync::Notify;

#[derive(Clone, Debug)]
struct Request {
    scope: LiveRuntimeScopeId,
    path: PathBuf,
    reason: SessionStartReason,
}

#[derive(Default)]
struct Factory {
    requests: Mutex<Vec<Request>>,
    revision: Arc<AtomicUsize>,
    failure: AtomicUsize,
    gate: Mutex<Option<Arc<Gate>>>,
}

#[derive(Default)]
struct Gate {
    entered: Notify,
    release: Notify,
}

impl Factory {
    fn manager(self: &Arc<Self>) -> MultiSessionManager {
        // Multiple managers deliberately forward into the very same factory,
        // rather than relying on independent state in factory clones.
        let factory = self.clone();
        MultiSessionManager::new(move |request| {
            let factory = factory.clone();
            async move { factory.prepare_generation(request).await }
        })
    }

    fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }

    fn last_scope(&self) -> LiveRuntimeScopeId {
        self.requests.lock().unwrap().last().unwrap().scope
    }
}

#[async_trait]
impl SessionGenerationFactory for Factory {
    async fn prepare_generation(
        &self,
        request: SessionGenerationRequest,
    ) -> Result<PreparedSessionGeneration, SessionError> {
        self.requests.lock().unwrap().push(Request {
            scope: request.runtime_scope,
            path: request.session_path,
            reason: request.reason,
        });
        let gate = self.gate.lock().unwrap().take();
        if let Some(gate) = gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        let failure = self.failure.load(Ordering::Acquire);
        if failure == 1 {
            return Err(SessionError::Runtime(
                "scope fixture rejected candidate".into(),
            ));
        }
        let cwd = if failure == 2 {
            // Return a complete candidate which fails the slot's later validation.
            request.cwd.join("wrong-workspace")
        } else {
            request.cwd
        };
        let runtime = PiRuntime::builder()
            .provider_plugin(ScriptedProviderPlugin::scripted([ScriptedTurn::Text(
                "scope fixture response".into(),
            )]))
            .agent_options(AgentOptions {
                cwd,
                provider_id: ProviderId::new("scripted"),
                model_id: ModelId::new("test"),
                ..AgentOptions::default()
            })
            .build()?;
        let revision = self.revision.clone();
        let baseline = revision.load(Ordering::Acquire);
        Ok(PreparedSessionGeneration::new(
            runtime,
            AgentSessionOptions::default().generation_freshness(Arc::new(move || {
                Ok(revision.load(Ordering::Acquire) != baseline)
            })),
        ))
    }
}

#[test]
fn runtime_scope_allocation_is_unique_under_concurrency() {
    let scopes = std::thread::scope(|scope| {
        let threads = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    (0..128)
                        .map(|_| LiveRuntimeScopeId::new())
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>();
        threads
            .into_iter()
            .flat_map(|thread| thread.join().unwrap())
            .collect::<HashSet<_>>()
    });
    assert_eq!(scopes.len(), 512);
    let fresh = LiveRuntimeScopeId::default();
    assert!(!scopes.contains(&fresh));
}

#[tokio::test]
async fn shared_factory_same_saved_path_isolates_managers_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("shared.jsonl");
    drop(SessionStore::create(&path, SessionHeader::new("saved", directory.path())).unwrap());
    let factory = Arc::new(Factory::default());
    let first_manager = factory.manager();
    let second_manager = factory.manager();
    let first = first_manager
        .open_session(&path, Default::default())
        .await
        .unwrap();
    let first_scope = factory.last_scope();
    let second = second_manager
        .open_session(&path, Default::default())
        .await
        .unwrap();
    let second_scope = factory.last_scope();
    assert_ne!(first_scope, second_scope);
    assert_eq!(first.id(), second.id());
    assert!(
        factory
            .requests()
            .iter()
            .all(|request| request.path == path)
    );

    first.reload().await.unwrap();
    assert_eq!(factory.last_scope(), first_scope);
    second.reload().await.unwrap();
    assert_eq!(factory.last_scope(), second_scope);
    first_manager.close_session(&first).await.unwrap();
    let reopened = first_manager
        .open_session(&path, Default::default())
        .await
        .unwrap();
    assert_ne!(factory.last_scope(), first_scope);
    assert_ne!(factory.last_scope(), second_scope);
    assert_eq!(reopened.path(), path);
    second.reload().await.unwrap();
    assert_eq!(factory.last_scope(), second_scope);
    first_manager.shutdown().await.unwrap();
    second_manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn reload_and_automatic_refresh_preserve_scope_without_new_wire_records() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("draft.jsonl");
    let factory = Arc::new(Factory::default());
    let manager = factory.manager();
    let session = manager
        .create_session(CreateSessionOptions::new(directory.path(), &path))
        .await
        .unwrap();
    let scope = factory.last_scope();
    let conversation = session.id();
    session.reload().await.unwrap();
    assert_eq!(factory.last_scope(), scope);
    assert!(!path.exists());

    factory.revision.fetch_add(1, Ordering::AcqRel);
    session
        .current()
        .prompt("refresh before submission")
        .await
        .unwrap();
    assert_eq!(session.id(), conversation);
    assert_eq!(factory.requests().len(), 3);
    assert!(
        factory
            .requests()
            .iter()
            .all(|request| request.scope == scope)
    );
    assert_eq!(factory.requests()[2].reason, SessionStartReason::Reload);
    let bytes = std::fs::read(&path).unwrap();
    let journal = std::str::from_utf8(&bytes).unwrap();
    assert!(!journal.contains("runtime_scope"));
    assert!(!journal.contains("runtimeScope"));
    assert!(!journal.contains("LiveRuntimeScopeId"));
    assert_eq!(SessionStore::read(&path).unwrap().header.version, 4);
    session.reload().await.unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    manager.shutdown().await.unwrap();

    let resumed = manager_for_replay(&factory, &path).await;
    assert_ne!(resumed.1, scope);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    resumed.0.shutdown().await.unwrap();
}

async fn manager_for_replay(
    factory: &Arc<Factory>,
    path: &std::path::Path,
) -> (MultiSessionManager, LiveRuntimeScopeId) {
    let manager = factory.manager();
    manager
        .open_session(path, Default::default())
        .await
        .unwrap();
    (manager, factory.last_scope())
}

#[tokio::test]
async fn new_resume_same_path_and_forks_allocate_fresh_scopes() {
    let directory = tempfile::tempdir().unwrap();
    let first_path = directory.path().join("first.jsonl");
    let second_path = directory.path().join("second.jsonl");
    let factory = Arc::new(Factory::default());
    let manager = factory.manager();
    let session = manager
        .create_session(CreateSessionOptions::new(directory.path(), &first_path))
        .await
        .unwrap();
    let mut scopes = HashSet::from([factory.last_scope()]);
    session
        .new_session(directory.path(), &second_path)
        .await
        .unwrap();
    assert!(scopes.insert(factory.last_scope()));
    assert!(!first_path.exists());
    assert!(!second_path.exists());
    let id = session.id();
    session.resume_session(&second_path).await.unwrap();
    assert_eq!(session.id(), id);
    assert!(scopes.insert(factory.last_scope()));
    assert!(
        !second_path.exists(),
        "same-path resume must keep the unsaved store"
    );

    let user = session
        .current()
        .store()
        .append_message(Message::User(UserMessage::text("branch", 1)))
        .unwrap();
    session.current().store().materialize().unwrap();
    for position in [ForkPosition::At, ForkPosition::Before] {
        session.fork_session(&user, position).await.unwrap();
        let scope = factory.last_scope();
        assert!(scopes.insert(scope));
        session.reload().await.unwrap();
        assert_eq!(factory.last_scope(), scope);
    }
    session.resume_session(&second_path).await.unwrap();
    assert!(scopes.insert(factory.last_scope()));
    assert_eq!(scopes.len(), 6);
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_preparation_and_post_factory_validation_keep_published_scope() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("current.jsonl");
    let factory = Arc::new(Factory::default());
    let manager = factory.manager();
    let session = manager
        .create_session(CreateSessionOptions::new(directory.path(), &path))
        .await
        .unwrap();
    let scope = factory.last_scope();
    let user = session
        .current()
        .store()
        .append_message(Message::User(UserMessage::text("branch", 1)))
        .unwrap();
    session.current().store().materialize().unwrap();
    for failure in [1, 2] {
        for transition in 0..4 {
            let current = session.current();
            factory.failure.store(failure, Ordering::Release);
            let result = match transition {
                0 => session
                    .new_session(directory.path(), directory.path().join("new.jsonl"))
                    .await
                    .map(|_| ()),
                1 => session.resume_session(&path).await.map(|_| ()),
                2 => session
                    .fork_session(&user, ForkPosition::At)
                    .await
                    .map(|_| ()),
                _ => session.reload().await,
            };
            assert!(result.is_err());
            let candidate = factory.last_scope();
            if transition == 3 {
                assert_eq!(candidate, scope);
            } else {
                assert_ne!(candidate, scope);
            }
            assert!(Arc::ptr_eq(&current, &session.current()));
            assert!(!current.is_closed());
            factory.failure.store(0, Ordering::Release);
            session.reload().await.unwrap();
            assert_eq!(factory.last_scope(), scope);
        }
    }
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn concurrent_replacements_publish_scopes_only_after_preparation() {
    let directory = tempfile::tempdir().unwrap();
    let first_path = directory.path().join("first.jsonl");
    let second_path = directory.path().join("second.jsonl");
    let third_path = directory.path().join("third.jsonl");
    let factory = Arc::new(Factory::default());
    let manager = factory.manager();
    let session = manager
        .create_session(CreateSessionOptions::new(directory.path(), &first_path))
        .await
        .unwrap();
    let original = session.current();
    let gate = Arc::new(Gate::default());
    *factory.gate.lock().unwrap() = Some(gate.clone());
    let first = tokio::spawn({
        let session = session.clone();
        let cwd = directory.path().to_owned();
        async move { session.new_session(cwd, second_path).await }
    });
    gate.entered.notified().await;
    assert!(Arc::ptr_eq(&original, &session.current()));
    assert_eq!(session.path(), first_path);
    let second = tokio::spawn({
        let session = session.clone();
        let cwd = directory.path().to_owned();
        let third_path = third_path.clone();
        async move { session.new_session(cwd, third_path).await }
    });
    gate.release.notify_one();
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert_eq!(session.path(), third_path);
    let requests = factory.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests
            .iter()
            .map(|request| request.scope)
            .collect::<HashSet<_>>()
            .len(),
        3
    );
    session.reload().await.unwrap();
    assert_eq!(factory.last_scope(), requests[2].scope);
    assert!(!first_path.exists());
    assert!(!third_path.exists());
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn abrupt_slot_drop_then_same_path_creation_never_reuses_scope() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("draft.jsonl");
    let factory = Arc::new(Factory::default());
    let manager = factory.manager();
    let session = manager
        .create_session(CreateSessionOptions::new(directory.path(), &path))
        .await
        .unwrap();
    let scope = factory.last_scope();
    let retained_executable = session.current().runtime().clone();
    drop(session);
    drop(manager);
    let manager = factory.manager();
    let session = manager
        .create_session(CreateSessionOptions::new(directory.path(), &path))
        .await
        .unwrap();
    assert_ne!(factory.last_scope(), scope);
    assert!(!path.exists());
    session.reload().await.unwrap();
    assert_ne!(factory.last_scope(), scope);
    drop(retained_executable);
    manager.shutdown().await.unwrap();
}
