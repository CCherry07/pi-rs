use std::sync::Arc;

use async_trait::async_trait;
use pi_agent::AgentOptions;
use pi_core::{ModelSelection, ThinkingLevel};
use pi_plugin::{Plugin, PresentationMode, ProviderPlugin};
use pi_runtime::{PiRuntime, SystemPrompt};
use pi_session::{
    AgentSessionOptions, InitialModelRequest, MultiSessionManager, PiPluginContext, PiSession,
    PluginContextBinding, PreparedSessionGeneration, RestoredToolSelection, SessionError,
    SessionGenerationFactory, SessionGenerationRequest,
};

type PluginFactory = Arc<dyn Fn() -> Result<Arc<dyn Plugin>, String> + Send + Sync>;
type ProviderFactory = Arc<dyn Fn() -> Result<Arc<dyn ProviderPlugin>, String> + Send + Sync>;

/// A domain-neutral agent harness using the same managed sessions as the Coding product.
pub struct AgentHarness {
    sessions: MultiSessionManager,
}

/// Explicit composition for a domain. No tools or provider plugins are installed implicitly.
///
/// Owns the configuration and implements [`SessionGenerationFactory`] to rebuild plugins for
/// each session generation. [`Self::build`] transfers ownership to the harness's session manager;
/// applications can also pass the builder directly to [`MultiSessionManager::new`].
pub struct AgentHarnessBuilder {
    model: ModelSelection,
    system_prompt: SystemPrompt,
    thinking_level: ThinkingLevel,
    max_tool_iterations: Option<usize>,
    active_tools: Option<Vec<String>>,
    session_options: AgentSessionOptions,
    plugins: Vec<PluginFactory>,
    providers: Vec<ProviderFactory>,
    binding: PluginContextBinding,
}

impl AgentHarness {
    /// Selects the model used for fresh sessions and the complete reusable base prompt.
    /// Saved sessions restore their model; a live model change survives reload.
    pub fn builder(model: ModelSelection, system_prompt: impl Into<String>) -> AgentHarnessBuilder {
        AgentHarnessBuilder {
            model,
            system_prompt: SystemPrompt::Final(system_prompt.into()),
            thinking_level: ThinkingLevel::Off,
            max_tool_iterations: AgentOptions::default().max_tool_iterations,
            active_tools: None,
            session_options: AgentSessionOptions::default(),
            plugins: Vec::new(),
            providers: Vec::new(),
            binding: PluginContextBinding::new(),
        }
    }

    /// Creates, resumes, forks, closes, and coordinates sessions through the shared manager.
    /// Callers choose the workspace and JSONL path explicitly for each new session.
    pub fn sessions(&self) -> &MultiSessionManager {
        &self.sessions
    }

    pub fn session_manager(&self) -> MultiSessionManager {
        self.sessions.clone()
    }
}

impl AgentHarnessBuilder {
    /// Supplies an exact prompt or a domain-owned factory using the runtime's existing
    /// generation preparation. Dynamic factories prepare once per generation; tool changes
    /// re-render those frozen inputs without reading resources again.
    pub fn system_prompt(mut self, prompt: SystemPrompt) -> Self {
        self.system_prompt = prompt;
        self
    }

    /// Each generation receives a fresh unified Agent/Session plugin instance.
    pub fn plugin_factory<F, P>(self, factory: F) -> Self
    where
        F: Fn() -> P + Send + Sync + 'static,
        P: Plugin + 'static,
    {
        self.try_plugin_factory(move || Ok::<P, std::convert::Infallible>(factory()))
    }

    /// A failed factory leaves the live session generation intact.
    pub fn try_plugin_factory<F, P, E>(self, factory: F) -> Self
    where
        F: Fn() -> Result<P, E> + Send + Sync + 'static,
        P: Plugin + 'static,
        E: std::fmt::Display,
    {
        self.try_plugin_arc_factory(move || {
            factory().map(|plugin| Arc::new(plugin) as Arc<dyn Plugin>)
        })
    }

    /// Accepts type-erased plugins from an explicitly configured loader or adapter.
    pub fn try_plugin_arc_factory<F, E>(mut self, factory: F) -> Self
    where
        F: Fn() -> Result<Arc<dyn Plugin>, E> + Send + Sync + 'static,
        E: std::fmt::Display,
    {
        self.plugins.push(Arc::new(move || {
            factory().map_err(|error| error.to_string())
        }));
        self
    }

    /// Provider implementations and catalogs keep their independent plugin contract.
    pub fn provider_plugin_factory<F, P>(self, factory: F) -> Self
    where
        F: Fn() -> P + Send + Sync + 'static,
        P: ProviderPlugin + 'static,
    {
        self.try_provider_plugin_factory(move || Ok::<P, std::convert::Infallible>(factory()))
    }

    pub fn try_provider_plugin_factory<F, P, E>(self, factory: F) -> Self
    where
        F: Fn() -> Result<P, E> + Send + Sync + 'static,
        P: ProviderPlugin + 'static,
        E: std::fmt::Display,
    {
        self.try_provider_plugin_arc_factory(move || {
            factory().map(|plugin| Arc::new(plugin) as Arc<dyn ProviderPlugin>)
        })
    }

    pub fn try_provider_plugin_arc_factory<F, E>(mut self, factory: F) -> Self
    where
        F: Fn() -> Result<Arc<dyn ProviderPlugin>, E> + Send + Sync + 'static,
        E: std::fmt::Display,
    {
        self.providers.push(Arc::new(move || {
            factory().map_err(|error| error.to_string())
        }));
        self
    }

    pub fn thinking_level(mut self, level: ThinkingLevel) -> Self {
        self.thinking_level = level;
        self
    }

    /// Overrides the core's tool-turn limit for every generation, including resumed sessions.
    /// None runs until the model finishes or the caller cancels; Some(0) permits no tool turns.
    pub fn max_tool_iterations(mut self, limit: Option<usize>) -> Self {
        self.max_tool_iterations = limit;
        self
    }

    /// Defaults to every tool registered by the supplied plugins. An explicit empty list
    /// disables all tools. Saved sessions retain their active selection when reopened.
    pub fn active_tools(mut self, mut tools: Vec<String>) -> Self {
        let mut seen = std::collections::HashSet::new();
        tools.retain(|tool| seen.insert(tool.clone()));
        self.active_tools = Some(tools);
        self
    }

    /// Supplies the shared session policies, including retry, context projection, and compaction.
    /// `initial_model` can explicitly override a saved model; otherwise resume uses the journal.
    pub fn session_options(mut self, options: AgentSessionOptions) -> Self {
        self.session_options = options;
        self
    }

    /// Assembly is lazy: factories run only when a session generation is prepared.
    /// Registration errors are returned from session creation, resume, or reload.
    pub fn build(self) -> AgentHarness {
        AgentHarness {
            sessions: MultiSessionManager::new(self),
        }
    }

    fn runtime(
        &self,
        request: &SessionGenerationRequest,
        context: Arc<PiPluginContext>,
    ) -> Result<PiRuntime, SessionError> {
        let model = request.reload_model.as_ref().unwrap_or(&self.model);
        let initial_tools = request
            .initial_state
            .as_ref()
            .map(|state| &state.active_tools)
            .or(self.active_tools.as_ref());
        let mut builder = PiRuntime::builder()
            .workspace(request.workspace.clone())
            .plugin_context(context)
            .system_prompt(self.system_prompt.clone())
            .agent_options(AgentOptions {
                provider_id: model.provider.clone(),
                model_id: model.model_id.clone(),
                thinking_level: self.thinking_level,
                max_tool_iterations: self.max_tool_iterations,
                cwd: request.workspace.cwd().to_path_buf(),
                active_tools: initial_tools.cloned().unwrap_or_default(),
                ..AgentOptions::default()
            });
        if let Some(restored) = &request.restored_configuration {
            match &restored.active_tools {
                RestoredToolSelection::Selected(tools) => {
                    let mut tools = tools.clone();
                    tools.extend(self.session_options.additional_active_tools.iter().cloned());
                    builder = builder.restored_active_tools(tools);
                }
                RestoredToolSelection::RuntimeDefault => {
                    builder = match initial_tools {
                        Some(tools) => builder.active_tools_with_restored_additions(
                            tools.clone(),
                            self.session_options.additional_active_tools.clone(),
                        ),
                        // All registered tools already include registered additions.
                        None => builder.all_tools_active(),
                    };
                }
            }
        } else if initial_tools.is_none() {
            builder = builder.all_tools_active();
        }
        for factory in &self.plugins {
            let factory = Arc::clone(factory);
            builder = builder.try_plugin_arc_factory(move || factory());
        }
        for factory in &self.providers {
            let factory = Arc::clone(factory);
            builder = builder.try_provider_plugin_arc_factory(move || factory());
        }
        let runtime = request.generation_overlay.apply_to(builder).build()?;
        if !runtime.has_provider(&model.provider) {
            return Err(SessionError::Runtime(format!(
                "provider {} is not registered",
                model.provider
            )));
        }
        Ok(runtime)
    }
}

#[async_trait]
impl SessionGenerationFactory for AgentHarnessBuilder {
    fn session_registered(&self, session: &PiSession) {
        self.binding.bind(session.clone());
    }

    async fn prepare_generation(
        &self,
        request: SessionGenerationRequest,
    ) -> Result<PreparedSessionGeneration, SessionError> {
        if request.workspace.cwd() != request.cwd {
            return Err(SessionError::Runtime(
                "generation cwd disagrees with workspace".into(),
            ));
        }
        let context = Arc::new(PiPluginContext::new(
            PresentationMode::Print,
            false,
            self.binding.clone(),
        ));
        let runtime = self.runtime(&request, context.clone())?;
        let mut options = self.session_options.clone();
        // Fresh sessions record the runtime's selected model immediately. Apply an explicit
        // options-level request before that header is prepared; reopen resolves it with the
        // saved model, and isolated sessions already supply their own initial selection.
        if request.restored_configuration.is_none()
            && request.initial_state.is_none()
            && options.initial_model != InitialModelRequest::default()
        {
            let selection = options
                .initial_model
                .clone()
                .select(&runtime)
                .map_err(|error| SessionError::Runtime(error.to_string()))?;
            options = options.initial_model_fallback_message(selection.fallback_message);
        }
        if let Some(model) = request.reload_model {
            options.initial_model =
                InitialModelRequest::default().requested(model.provider, model.model_id.as_str());
        }
        Ok(PreparedSessionGeneration::new(runtime, options)
            .bind_session(move |session| context.bind_generation_session(session)))
    }
}
