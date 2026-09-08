use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use async_trait::async_trait;
use pi_agent::AgentOptions;
use pi_core::{ModelId, PluginContext, PresentationMode, ProviderId};
use pi_plugin_schedule::{ScheduleOptions, SchedulePlugin, ScheduleSessionPlugin};
use pi_runtime::{PiRuntime, SystemPrompt};
use pi_session::{
    AgentSession, AgentSessionOptions, AgentSessionRuntimeFactory, AgentSessionRuntimeRequest,
    AgentSessionRuntimeTarget, MultiSessionManager, PiPluginContext, PiSession,
    PluginContextBinding, PreparedAgentSession, SessionError, SessionPlugins,
};
use pi_test_support::{ScriptedProvider, ScriptedProviderPlugin, ScriptedTurn};
use serde_json::{Value, json};

#[derive(Clone)]
struct Factory {
    agent_dir: PathBuf,
    binding: PluginContextBinding,
    providers: Arc<Mutex<Vec<Arc<ScriptedProvider>>>>,
    child_turn: ScriptedTurn,
    parent_turn: ScriptedTurn,
    fail: Arc<AtomicBool>,
    trusted: bool,
}

impl Factory {
    fn new(agent_dir: PathBuf, child_turn: ScriptedTurn) -> Self {
        Self {
            agent_dir,
            binding: PluginContextBinding::new(),
            providers: Arc::default(),
            child_turn,
            parent_turn: ScriptedTurn::Text("parent response".into()),
            fail: Arc::default(),
            trusted: true,
        }
    }
}

#[async_trait]
impl AgentSessionRuntimeFactory for Factory {
    fn session_registered(&self, session: &PiSession) {
        self.binding.bind(session.clone());
    }

    async fn prepare(
        &self,
        request: AgentSessionRuntimeRequest,
    ) -> Result<PreparedAgentSession, SessionError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(SessionError::Runtime("candidate rejected".into()));
        }
        let (cwd, path, reused, create) = match request.target {
            AgentSessionRuntimeTarget::Create { cwd, path, .. } => (cwd, path, None, true),
            AgentSessionRuntimeTarget::Reuse { log } => (
                log.header().cwd.clone(),
                log.path().to_path_buf(),
                Some(log),
                false,
            ),
            AgentSessionRuntimeTarget::Open { path } => {
                let (log, document) = pi_session::SessionLog::open(&path)?;
                (document.header.cwd, path, Some(log), false)
            }
        };
        let child = request.initial_state.is_some();
        let provider = ScriptedProviderPlugin::scripted(if child {
            vec![self.child_turn.clone()]
        } else {
            vec![
                self.parent_turn.clone(),
                ScriptedTurn::Text("scheduled".into()),
            ]
        });
        self.providers.lock().unwrap().push(provider.provider());
        let context = Arc::new(PiPluginContext::new(
            PresentationMode::Print,
            self.trusted,
            self.binding.clone(),
        ));
        let access: Arc<dyn PluginContext> = context.clone();
        let options = ScheduleOptions::new(&cwd, &self.agent_dir, self.trusted);
        let builder = PiRuntime::builder()
            .plugin_context(access)
            .provider_plugin(provider)
            .system_prompt(SystemPrompt::Pi(Box::default()))
            .agent_plugin_factory({
                let options = options.clone();
                move || SchedulePlugin::new(options.clone())
            })
            .agent_options(AgentOptions {
                cwd,
                provider_id: ProviderId::new("scripted"),
                model_id: ModelId::new("test"),
                active_tools: vec!["schedule".into()],
                ..AgentOptions::default()
            });
        let runtime = request.generation_overlay.apply_to(builder).build()?;
        if let Some(initial) = request.initial_state {
            initial.apply_to(&runtime)?;
        }
        let session_options = AgentSessionOptions::default().plugins(
            SessionPlugins::new()
                .plugin_factory(move || ScheduleSessionPlugin::new(options.clone())),
        );
        let prepared = if create {
            AgentSession::prepare_create_with_options(runtime, path, session_options).await?
        } else {
            AgentSession::prepare_reuse_with_options(runtime, reused.unwrap(), session_options)
                .await?
        };
        context.bind_generation_session(prepared.session());
        Ok(prepared)
    }
}

fn database(agent_dir: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(agent_dir.join("schedule/jobs.json")).unwrap()).unwrap()
}

async fn wait_until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("scheduler did not reach the expected state");
}

async fn create(root: &PiSession, extras: Value) {
    let mut job =
        json!({"name":"check", "prompt":"inspect build 42", "schedule":"in 1s", "notify":false});
    job.as_object_mut()
        .unwrap()
        .extend(extras.as_object().unwrap().clone());
    root.current()
        .submit(format!("/schedule create {job}"))
        .await
        .unwrap();
}

#[tokio::test]
async fn tool_creation_and_provider_failure_are_recorded() {
    let directory = tempfile::tempdir().unwrap();
    let agent_dir = directory.path().join("agent");
    let mut factory = Factory::new(
        agent_dir.clone(),
        ScriptedTurn::Error("provider unavailable".into()),
    );
    factory.parent_turn = ScriptedTurn::ToolCalls(vec![pi_core::ToolCall::new(
        pi_core::ToolCallId::new("schedule-1"),
        "schedule",
        json!({"action":"create","name":"watch","schedule":"in 1s","prompt":"inspect", "notify":false}),
    )]);
    let manager = MultiSessionManager::new(factory);
    let root = manager
        .create_session(directory.path(), directory.path().join("parent.jsonl"))
        .await
        .unwrap();
    root.current().submit("Schedule a check").await.unwrap();
    wait_until(|| database(&agent_dir)["runs"][0]["status"] == "failed").await;
    assert!(
        database(&agent_dir)["runs"][0]["error"]
            .as_str()
            .unwrap()
            .contains("provider unavailable")
    );
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn due_jobs_wait_for_foreground_work_to_settle() {
    let directory = tempfile::tempdir().unwrap();
    let agent_dir = directory.path().join("agent");
    let mut factory = Factory::new(
        agent_dir.clone(),
        ScriptedTurn::Text("background done".into()),
    );
    factory.parent_turn = ScriptedTurn::WaitForAbort;
    let providers = factory.providers.clone();
    let manager = MultiSessionManager::new(factory);
    let root = manager
        .create_session(directory.path(), directory.path().join("parent.jsonl"))
        .await
        .unwrap();
    create(&root, json!({})).await;
    let foreground = tokio::spawn({
        let session = root.current();
        async move { session.submit("foreground work").await }
    });
    wait_until(|| !providers.lock().unwrap()[0].requests().is_empty()).await;
    tokio::time::sleep(Duration::from_millis(2100)).await;
    assert!(database(&agent_dir)["runs"].as_array().unwrap().is_empty());
    root.abort();
    foreground.await.unwrap().unwrap();
    wait_until(|| database(&agent_dir)["runs"][0]["status"] == "completed").await;
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn scheduled_work_uses_fresh_session_and_never_reaches_parent_or_recurses() {
    let directory = tempfile::tempdir().unwrap();
    let agent_dir = directory.path().join("agent");
    let factory = Factory::new(agent_dir.clone(), ScriptedTurn::Text("build passed".into()));
    let providers = factory.providers.clone();
    let manager = MultiSessionManager::new(factory);
    let root = manager
        .create_session(directory.path(), directory.path().join("parent.jsonl"))
        .await
        .unwrap();
    let other = manager
        .create_session(directory.path(), directory.path().join("other.jsonl"))
        .await
        .unwrap();
    create(&root, json!({})).await;
    wait_until(|| {
        database(&agent_dir)["runs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|run| run["status"] == "completed")
    })
    .await;
    let db = database(&agent_dir);
    assert_eq!(db["runs"].as_array().unwrap().len(), 1);
    assert_eq!(db["runs"][0]["output"], "build passed");
    assert!(db["runs"][0]["session_id"].as_str().is_some());
    assert!(!root.current().log().is_materialized());
    assert!(!other.current().log().is_materialized());
    let providers = providers.lock().unwrap().clone();
    assert_eq!(
        providers.iter().map(|p| p.requests().len()).sum::<usize>(),
        1
    );
    let request = providers.iter().flat_map(|p| p.requests()).next().unwrap();
    assert_eq!(request.messages.len(), 1);
    let serialized = serde_json::to_string(&request.messages).unwrap();
    assert!(serialized.contains("inspect build 42"));
    assert!(!serialized.contains("schedule create"));
    assert!(request.tools.iter().all(|tool| tool.name != "schedule"));
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn successful_reload_cancels_active_work_and_does_not_replay_once() {
    let directory = tempfile::tempdir().unwrap();
    let agent_dir = directory.path().join("agent");
    let factory = Factory::new(agent_dir.clone(), ScriptedTurn::WaitForAbort);
    let providers = factory.providers.clone();
    let manager = MultiSessionManager::new(factory);
    let root = manager
        .create_session(directory.path(), directory.path().join("parent.jsonl"))
        .await
        .unwrap();
    create(&root, json!({})).await;
    wait_until(|| {
        providers
            .lock()
            .unwrap()
            .iter()
            .any(|p| !p.requests().is_empty())
    })
    .await;
    tokio::time::timeout(Duration::from_secs(8), root.reload())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(database(&agent_dir)["runs"][0]["status"], "aborted");
    assert_eq!(database(&agent_dir)["jobs"][0]["enabled"], false);
    assert!(!root.current().log().is_materialized());
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_reload_keeps_old_scheduler_and_timeout_is_recorded() {
    let directory = tempfile::tempdir().unwrap();
    let agent_dir = directory.path().join("agent");
    let factory = Factory::new(agent_dir.clone(), ScriptedTurn::WaitForAbort);
    let fail = factory.fail.clone();
    let providers = factory.providers.clone();
    let manager = MultiSessionManager::new(factory);
    let root = manager
        .create_session(directory.path(), directory.path().join("parent.jsonl"))
        .await
        .unwrap();
    create(&root, json!({"timeout_seconds":2})).await;
    wait_until(|| {
        providers
            .lock()
            .unwrap()
            .iter()
            .any(|p| !p.requests().is_empty())
    })
    .await;
    fail.store(true, Ordering::SeqCst);
    assert!(root.reload().await.is_err());
    fail.store(false, Ordering::SeqCst);
    wait_until(|| database(&agent_dir)["runs"][0]["status"] == "timed_out").await;
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn schedules_survive_quit_and_untrusted_project_store_is_ignored() {
    let directory = tempfile::tempdir().unwrap();
    let agent_dir = directory.path().join("agent");
    let factory = Factory::new(agent_dir.clone(), ScriptedTurn::Text("restarted".into()));
    let manager = MultiSessionManager::new(factory.clone());
    let root = manager
        .create_session(directory.path(), directory.path().join("first.jsonl"))
        .await
        .unwrap();
    create(&root, json!({"paused":true})).await;
    let id = database(&agent_dir)["jobs"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    manager.shutdown().await.unwrap();
    let mut factory = factory;
    factory.trusted = false;
    std::fs::create_dir_all(directory.path().join(".pi/schedule")).unwrap();
    std::fs::write(
        directory.path().join(".pi/schedule/jobs.json"),
        "invalid JSON that must not be read",
    )
    .unwrap();
    let manager = MultiSessionManager::new(factory);
    let root = manager
        .create_session(directory.path(), directory.path().join("second.jsonl"))
        .await
        .unwrap();
    assert!(
        root.current()
            .submit("/schedule list project")
            .await
            .is_err()
    );
    root.current()
        .submit(format!("/schedule run_now {id}"))
        .await
        .unwrap();
    wait_until(|| database(&agent_dir)["runs"][0]["status"] == "completed").await;
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_while_child_is_running_finishes_without_manager_gate_deadlock() {
    let directory = tempfile::tempdir().unwrap();
    let agent_dir = directory.path().join("agent");
    let factory = Factory::new(agent_dir.clone(), ScriptedTurn::WaitForAbort);
    let providers = factory.providers.clone();
    let manager = MultiSessionManager::new(factory);
    let root = manager
        .create_session(directory.path(), directory.path().join("parent.jsonl"))
        .await
        .unwrap();
    create(&root, json!({})).await;
    wait_until(|| {
        providers
            .lock()
            .unwrap()
            .iter()
            .any(|p| !p.requests().is_empty())
    })
    .await;
    tokio::time::timeout(Duration::from_secs(8), manager.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert_ne!(database(&agent_dir)["runs"][0]["status"], "running");
}
