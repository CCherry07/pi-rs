use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pi_agent::AgentOptions;
use pi_plugin::{
    AgentPluginContext, BeforeAgentStartEvent, BeforeAgentStartPatch, Plugin, PluginError,
    PluginFactory, PluginId, PluginScope, PrepareContext, PrepareError, RegisterContext,
    SessionHook, SessionInfoChangedEvent, SessionPluginContext, SessionShutdownEvent,
    SessionStartEvent,
};
use pi_runtime::PiRuntime;
use pi_session::{
    AgentSessionOptions, MultiSessionManager, PreparedSessionGeneration, SessionError,
    SessionGenerationFactory, SessionGenerationRequest,
};
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};
use serde::Deserialize;

#[derive(Default)]
struct Observations {
    prepared: AtomicUsize,
    events: Mutex<Vec<(usize, &'static str)>>,
}

#[derive(Clone, Default, Deserialize)]
struct Options {
    enabled: Option<bool>,
    #[serde(skip)]
    observations: Arc<Observations>,
}

struct UnifiedPlugin {
    instance: usize,
    observations: Arc<Observations>,
    started: AtomicUsize,
}

impl PluginFactory for UnifiedPlugin {
    type Options = Options;

    fn prepare(context: &PrepareContext, options: Options) -> Result<Option<Self>, PrepareError> {
        let instance = options.observations.prepared.fetch_add(1, Ordering::SeqCst) + 1;
        let file: Options = serde_json::from_slice(
            &std::fs::read(context.package_dir().join("config.json"))
                .map_err(|error| PrepareError::Initialization(error.to_string()))?,
        )
        .map_err(|error| PrepareError::InvalidOptions(error.to_string()))?;
        if !options.enabled.or(file.enabled).unwrap_or(true) {
            return Ok(None);
        }
        Ok(Some(Self {
            instance,
            observations: options.observations,
            started: AtomicUsize::new(0),
        }))
    }
}

impl UnifiedPlugin {
    fn record(&self, event: &'static str) {
        self.observations
            .events
            .lock()
            .unwrap()
            .push((self.instance, event));
    }
}

#[pi_plugin::plugin]
impl Plugin for UnifiedPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("unified-fixture")
    }

    fn register(&self, _: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
        self.record("register");
        Ok(())
    }

    async fn session_start(
        &self,
        _: &SessionPluginContext,
        _: &SessionStartEvent,
    ) -> Result<(), PluginError> {
        self.started.fetch_add(1, Ordering::SeqCst);
        self.record("start");
        Ok(())
    }

    async fn before_agent_start(
        &self,
        _: AgentPluginContext,
        _: BeforeAgentStartEvent,
    ) -> Result<BeforeAgentStartPatch, PluginError> {
        assert_eq!(
            self.started.load(Ordering::SeqCst),
            1,
            "Session and Agent must use the same instance"
        );
        self.record("agent");
        Ok(BeforeAgentStartPatch::default())
    }

    async fn session_shutdown(
        &self,
        _: &SessionPluginContext,
        _: &SessionShutdownEvent,
    ) -> Result<(), PluginError> {
        self.record("shutdown");
        Ok(())
    }

    async fn session_info_changed(
        &self,
        _: &SessionPluginContext,
        _: &SessionInfoChangedEvent,
    ) -> Result<(), PluginError> {
        Err(PluginError::Failure("observer fixture".into()))
    }
}

struct Factory(Options);

#[async_trait::async_trait]
impl SessionGenerationFactory for Factory {
    async fn prepare_generation(
        &self,
        request: SessionGenerationRequest,
    ) -> Result<PreparedSessionGeneration, SessionError> {
        let cwd = request.cwd;
        let context = PrepareContext::new(&cwd, &cwd, &cwd, &cwd, PluginScope::ExplicitPath, 0);
        let runtime = request
            .generation_overlay
            .apply_to(PiRuntime::builder())
            .prepare_plugin::<UnifiedPlugin>(context, self.0.clone())
            .provider_plugin(ScriptedProviderPlugin::scripted([
                ScriptedTurn::Text("one".into()),
                ScriptedTurn::Text("two".into()),
            ]))
            .agent_options(AgentOptions {
                cwd,
                provider_id: "scripted".into(),
                model_id: "test".into(),
                ..AgentOptions::default()
            })
            .build()?;
        Ok(PreparedSessionGeneration::new(
            runtime,
            AgentSessionOptions::default(),
        ))
    }
}

#[tokio::test]
async fn one_preparation_and_instance_span_both_hook_families_and_transactional_reload() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config.json");
    std::fs::write(&config, r#"{"enabled":true}"#).unwrap();
    let observations = Arc::new(Observations::default());
    let manager = MultiSessionManager::new(Factory(Options {
        observations: observations.clone(),
        ..Options::default()
    }));
    let session = manager
        .create_session(directory.path(), directory.path().join("session.jsonl"))
        .await
        .unwrap();
    session.current().prompt("first").await.unwrap();
    assert_eq!(observations.prepared.load(Ordering::SeqCst), 1);
    assert_eq!(
        *observations.events.lock().unwrap(),
        [(1, "register"), (1, "start"), (1, "agent")]
    );

    let previous = session.current();
    previous
        .set_name(Some("shared driver".into()))
        .await
        .unwrap();
    let driver = previous.runtime().plugin_driver();
    // Product callers see Session failures through the same runtime diagnostic API.
    assert_eq!(
        previous.runtime().plugin_diagnostics(),
        driver.diagnostics()
    );
    let diagnostics = driver.take_diagnostics();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].plugin_id.as_str(), "unified-fixture");
    assert_eq!(
        diagnostics[0].generation,
        Some(previous.runtime().generation())
    );
    assert_eq!(diagnostics[0].hook, SessionHook::InfoChanged.into());
    assert!(driver.diagnostics().is_empty());

    std::fs::write(&config, "invalid json").unwrap();
    assert!(session.reload().await.is_err());
    assert!(Arc::ptr_eq(&previous, &session.current()));
    session.current().prompt("still active").await.unwrap();
    assert_eq!(
        observations.events.lock().unwrap().last(),
        Some(&(1, "agent"))
    );

    std::fs::write(&config, r#"{"enabled":true}"#).unwrap();
    session.reload().await.unwrap();
    session.current().prompt("replacement").await.unwrap();
    assert_eq!(
        &observations.events.lock().unwrap()[4..],
        &[(3, "register"), (1, "shutdown"), (3, "start"), (3, "agent")]
    );

    std::fs::write(&config, r#"{"enabled":false}"#).unwrap();
    session.reload().await.unwrap();
    session.current().prompt("disabled").await.unwrap();
    assert!(session.current().runtime().plugin_order().is_empty());
    assert_eq!(
        observations.events.lock().unwrap().last(),
        Some(&(3, "shutdown"))
    );
    assert_eq!(observations.prepared.load(Ordering::SeqCst), 4);
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn explicit_override_can_enable_a_plugin_disabled_in_its_own_config() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("config.json"), r#"{"enabled":false}"#).unwrap();
    let manager = MultiSessionManager::new(Factory(Options {
        enabled: Some(true),
        ..Options::default()
    }));
    let session = manager
        .create_session(directory.path(), directory.path().join("session.jsonl"))
        .await
        .unwrap();
    assert_eq!(
        session.current().runtime().plugin_order(),
        [PluginId::new("unified-fixture")]
    );
    manager.shutdown().await.unwrap();
}
