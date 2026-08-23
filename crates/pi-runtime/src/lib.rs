#![forbid(unsafe_code)]

use std::sync::{Arc, Mutex, RwLock};

use futures::StreamExt;
use pi_agent::{
    Agent, AgentConfigurationPatch, AgentLoopOutcome, AgentOptions, AgentRestoreState,
    AgentRuntime, PromptInput, StreamAssembler,
};
use pi_core::{
    AbortHandle, AbortSignal, AgentPlugin, AssistantMessage, CommandContext, CommandOutcome,
    CommandSpec, InputEvent, InputPatch, Message, ModelId, ModelSpec, PluginId,
    ProviderCallContext, ProviderId, ProviderPlugin, ProviderPluginDriver, ProviderRequest,
    RegistriesBuilder, ThinkingLevel,
};
use pi_prompt::{BuildSystemPromptOptions, build_system_prompt};
use pi_resources::{ResourceDiagnostic, ResourceLoaderOptions, load_resources};

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("runtime build failed: {0}")]
    Build(String),
    #[error("agent failed: {0}")]
    Agent(String),
    #[error("unknown tools: {0}")]
    UnknownTools(String),
    #[error("dynamic prompt reconfiguration is unavailable for Final system prompts")]
    FinalPromptIsStatic,
    #[error("command {command} failed: {message}")]
    Command { command: String, message: String },
    #[error("input processing failed: {0}")]
    Input(String),
    #[error("runtime is busy with another operation")]
    Busy,
    #[error("provider operation failed: {0}")]
    Provider(String),
    #[error("provider stream assembly failed: {0}")]
    Assembly(String),
    #[error("runtime operation was aborted")]
    Aborted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReloadReport {
    pub previous_generation: u64,
    pub generation: u64,
    pub plugin_order: Vec<PluginId>,
    pub provider_plugin_order: Vec<PluginId>,
    pub resource_diagnostics: Vec<ResourceDiagnostic>,
}

pub struct RuntimePromptOutcome {
    pub generation: u64,
    pub base_system_prompt: String,
    pub active_tools: Vec<String>,
    pub prompt_options: Option<BuildSystemPromptOptions>,
    pub outcome: AgentLoopOutcome,
}

#[derive(Debug, Clone)]
pub struct RuntimeCompletionRequest {
    pub system_prompt: String,
    pub messages: Vec<Message>,
    pub thinking_level: ThinkingLevel,
    pub max_output_tokens: Option<u64>,
}

pub enum TextSubmissionOutcome {
    Handled,
    Agent(Box<RuntimePromptOutcome>),
}

/// Command/input-hook output prepared for delivery to an already active run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueuedTextOutcome {
    Handled,
    Message {
        generation: u64,
        display_text: String,
        text: String,
    },
}

/// A text submission whose command and input-hook preprocessing has completed
/// under a generation lease, but whose agent run has not started yet.
pub enum PreparedTextSubmission {
    Handled,
    Agent(PreparedRuntimePrompt),
}

pub struct PreparedRuntimePrompt {
    runtime: PiRuntime,
    _reload_guard: tokio::sync::OwnedMutexGuard<()>,
    generation: u64,
    display_text: String,
    text: String,
}

impl PreparedRuntimePrompt {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn display_text(&self) -> &str {
        &self.display_text
    }

    pub async fn run(self) -> Result<RuntimePromptOutcome, RuntimeError> {
        self.runtime
            .prompt_recorded_locked(PromptInput::Text(self.text))
            .await
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeRestoreState {
    pub provider_id: ProviderId,
    pub model_id: ModelId,
    pub thinking_level: ThinkingLevel,
    pub active_tools: Vec<String>,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone)]
pub enum SystemPrompt {
    /// Use this exact final prompt without Pi-style assembly.
    Final(String),
    /// Assemble the base prompt from active tools, context, and Pi defaults.
    /// Plugins may contribute generation-local content in `before_agent_start`.
    Pi(Box<BuildSystemPromptOptions>),
}

type PluginFactory = Arc<dyn Fn() -> Result<Arc<dyn AgentPlugin>, String> + Send + Sync>;
type ProviderPluginFactory = Arc<dyn Fn() -> Result<Arc<dyn ProviderPlugin>, String> + Send + Sync>;

enum PluginSource {
    Pinned(Arc<dyn AgentPlugin>),
    Factory(PluginFactory),
}

impl PluginSource {
    fn load(&self) -> Result<Arc<dyn AgentPlugin>, String> {
        match self {
            Self::Pinned(plugin) => Ok(Arc::clone(plugin)),
            Self::Factory(factory) => factory(),
        }
    }
}

enum ProviderPluginSource {
    Pinned(Arc<dyn ProviderPlugin>),
    Factory(ProviderPluginFactory),
}

impl ProviderPluginSource {
    fn load(&self) -> Result<Arc<dyn ProviderPlugin>, String> {
        match self {
            Self::Pinned(plugin) => Ok(Arc::clone(plugin)),
            Self::Factory(factory) => factory(),
        }
    }
}

pub struct PiRuntimeBuilder {
    plugin_sources: Vec<PluginSource>,
    provider_plugin_sources: Vec<ProviderPluginSource>,
    agent_options: AgentOptions,
    system_prompt: Option<SystemPrompt>,
    resources: Option<ResourceLoaderOptions>,
}

impl Default for PiRuntimeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl PiRuntimeBuilder {
    pub fn new() -> Self {
        Self {
            plugin_sources: Vec::new(),
            provider_plugin_sources: Vec::new(),
            agent_options: AgentOptions::default(),
            system_prompt: None,
            resources: None,
        }
    }

    pub fn agent_plugin(mut self, plugin: impl AgentPlugin + 'static) -> Self {
        self.plugin_sources
            .push(PluginSource::Pinned(Arc::new(plugin)));
        self
    }

    pub fn agent_plugin_arc(mut self, plugin: Arc<dyn AgentPlugin>) -> Self {
        self.plugin_sources.push(PluginSource::Pinned(plugin));
        self
    }

    /// Registers a reusable factory. A fresh plugin instance is created for
    /// the initial generation and every subsequent runtime reload.
    pub fn agent_plugin_factory<F, P>(mut self, factory: F) -> Self
    where
        F: Fn() -> P + Send + Sync + 'static,
        P: AgentPlugin + 'static,
    {
        self.plugin_sources
            .push(PluginSource::Factory(Arc::new(move || {
                Ok(Arc::new(factory()))
            })));
        self
    }

    /// Registers a reusable fallible factory. Failed preparation aborts the
    /// reload before the active generation is changed.
    pub fn try_agent_plugin_factory<F, P, E>(mut self, factory: F) -> Self
    where
        F: Fn() -> Result<P, E> + Send + Sync + 'static,
        P: AgentPlugin + 'static,
        E: std::fmt::Display,
    {
        self.plugin_sources
            .push(PluginSource::Factory(Arc::new(move || {
                factory()
                    .map(|plugin| Arc::new(plugin) as Arc<dyn AgentPlugin>)
                    .map_err(|error| error.to_string())
            })));
        self
    }

    /// Registers a type-erased, fallible agent plugin factory.
    ///
    /// Dynamic plugin adapters use this seam to retain generation-local
    /// reconstruction without exposing the runtime's internal source type.
    pub fn try_agent_plugin_arc_factory<F, E>(mut self, factory: F) -> Self
    where
        F: Fn() -> Result<Arc<dyn AgentPlugin>, E> + Send + Sync + 'static,
        E: std::fmt::Display,
    {
        self.plugin_sources
            .push(PluginSource::Factory(Arc::new(move || {
                factory().map_err(|error| error.to_string())
            })));
        self
    }

    pub fn provider_plugin(mut self, plugin: impl ProviderPlugin + 'static) -> Self {
        self.provider_plugin_sources
            .push(ProviderPluginSource::Pinned(Arc::new(plugin)));
        self
    }

    pub fn provider_plugin_arc(mut self, plugin: Arc<dyn ProviderPlugin>) -> Self {
        self.provider_plugin_sources
            .push(ProviderPluginSource::Pinned(plugin));
        self
    }

    /// Registers a reusable provider plugin factory. A fresh provider plugin
    /// instance is prepared for the initial generation and every reload.
    pub fn provider_plugin_factory<F, P>(mut self, factory: F) -> Self
    where
        F: Fn() -> P + Send + Sync + 'static,
        P: ProviderPlugin + 'static,
    {
        self.provider_plugin_sources
            .push(ProviderPluginSource::Factory(Arc::new(move || {
                Ok(Arc::new(factory()))
            })));
        self
    }

    /// Registers a fallible provider plugin factory. A failed preparation
    /// leaves the active runtime generation unchanged.
    pub fn try_provider_plugin_factory<F, P, E>(mut self, factory: F) -> Self
    where
        F: Fn() -> Result<P, E> + Send + Sync + 'static,
        P: ProviderPlugin + 'static,
        E: std::fmt::Display,
    {
        self.provider_plugin_sources
            .push(ProviderPluginSource::Factory(Arc::new(move || {
                factory()
                    .map(|plugin| Arc::new(plugin) as Arc<dyn ProviderPlugin>)
                    .map_err(|error| error.to_string())
            })));
        self
    }

    /// Registers a type-erased, fallible provider plugin factory.
    ///
    /// This is the provider/catalog counterpart to
    /// [`Self::try_agent_plugin_arc_factory`].
    pub fn try_provider_plugin_arc_factory<F, E>(mut self, factory: F) -> Self
    where
        F: Fn() -> Result<Arc<dyn ProviderPlugin>, E> + Send + Sync + 'static,
        E: std::fmt::Display,
    {
        self.provider_plugin_sources
            .push(ProviderPluginSource::Factory(Arc::new(move || {
                factory().map_err(|error| error.to_string())
            })));
        self
    }

    pub fn agent_options(mut self, options: AgentOptions) -> Self {
        self.agent_options = options;
        self
    }

    /// Configures either an exact final prompt or Pi-style prompt assembly.
    /// Without this, `AgentOptions.system_prompt` remains the final low-level prompt.
    pub fn system_prompt(mut self, system_prompt: SystemPrompt) -> Self {
        self.system_prompt = Some(system_prompt);
        self
    }

    /// Loads generic Pi prompt and project-context resources and injects them
    /// into Pi-style prompt assembly. Feature-specific resources belong to the
    /// plugin that consumes them.
    /// Resource `cwd` is always synchronized to `AgentOptions.cwd`.
    pub fn resources(mut self, resources: ResourceLoaderOptions) -> Self {
        self.resources = Some(resources);
        self
    }

    pub fn build(mut self) -> Result<PiRuntime, RuntimeError> {
        let cwd = self.agent_options.cwd.clone();
        let blueprint = Arc::new(RuntimeBlueprint {
            plugin_sources: self.plugin_sources,
            provider_plugin_sources: self.provider_plugin_sources,
            system_prompt: self.system_prompt,
            fallback_system_prompt: self.agent_options.system_prompt.clone(),
            resources: self.resources,
            cwd: cwd.clone(),
        });
        let generation = Arc::new(build_generation(
            &blueprint,
            1,
            &self.agent_options.active_tools,
        )?);
        self.agent_options.system_prompt = generation.agent.system_prompt().to_string();
        let agent = Agent::with_runtime(self.agent_options, Arc::clone(&generation.agent));
        Ok(PiRuntime {
            agent,
            cwd: Arc::new(cwd),
            blueprint,
            generation: Arc::new(RwLock::new(generation)),
            reload_lock: Arc::new(tokio::sync::Mutex::new(())),
        })
    }
}

struct RuntimeBlueprint {
    plugin_sources: Vec<PluginSource>,
    provider_plugin_sources: Vec<ProviderPluginSource>,
    system_prompt: Option<SystemPrompt>,
    fallback_system_prompt: String,
    resources: Option<ResourceLoaderOptions>,
    cwd: std::path::PathBuf,
}

struct RuntimeGeneration {
    agent: Arc<AgentRuntime>,
    provider_plugins: Arc<ProviderPluginDriver>,
    prompt_options: Mutex<Option<BuildSystemPromptOptions>>,
    resource_options: Option<ResourceLoaderOptions>,
    resource_diagnostics: Vec<ResourceDiagnostic>,
}

fn build_generation(
    blueprint: &RuntimeBlueprint,
    generation: u64,
    active_tools: &[String],
) -> Result<RuntimeGeneration, RuntimeError> {
    let mut system_prompt = blueprint.system_prompt.clone();
    let mut diagnostics = Vec::new();
    let mut applied_resources = None;
    if let Some(mut resources) = blueprint.resources.clone() {
        if matches!(system_prompt, Some(SystemPrompt::Final(_))) {
            return Err(RuntimeError::Build(
                "resources require Pi-style system prompt assembly; Final prompt cannot accept resources"
                    .to_string(),
            ));
        }
        resources.cwd = blueprint.cwd.clone();
        let loaded = load_resources(&resources);
        diagnostics = loaded.diagnostics.clone();
        let prompt = system_prompt.get_or_insert_with(|| SystemPrompt::Pi(Box::default()));
        let SystemPrompt::Pi(prompt) = prompt else {
            unreachable!("Final prompt rejected above")
        };
        loaded.apply_to_prompt(prompt);
        applied_resources = Some(resources);
    }

    let mut plugins = Vec::with_capacity(blueprint.plugin_sources.len());
    for (index, source) in blueprint.plugin_sources.iter().enumerate() {
        plugins.push(source.load().map_err(|message| {
            RuntimeError::Build(format!("plugin source {index} failed: {message}"))
        })?);
    }
    let mut provider_plugins = Vec::with_capacity(blueprint.provider_plugin_sources.len());
    for (index, source) in blueprint.provider_plugin_sources.iter().enumerate() {
        provider_plugins.push(source.load().map_err(|message| {
            RuntimeError::Build(format!("provider plugin source {index} failed: {message}"))
        })?);
    }
    let (driver, provider_driver, registries) = RegistriesBuilder::new()
        .register_plugin_sets(plugins, provider_plugins)
        .map_err(|error| RuntimeError::Build(error.to_string()))?;
    let driver = Arc::new(driver);
    let provider_driver = Arc::new(provider_driver);
    let registries = Arc::new(registries);

    let (assembled_prompt, prompt_options) = match system_prompt {
        Some(SystemPrompt::Final(prompt)) => (prompt, None),
        Some(SystemPrompt::Pi(mut prompt)) => {
            let assembled =
                assemble_prompt(&mut prompt, active_tools, &blueprint.cwd, &registries)?;
            (assembled, Some(*prompt))
        }
        None => (blueprint.fallback_system_prompt.clone(), None),
    };
    Ok(RuntimeGeneration {
        agent: Arc::new(AgentRuntime::new(
            generation,
            assembled_prompt,
            registries,
            driver,
            Arc::clone(&provider_driver),
        )),
        provider_plugins: provider_driver,
        prompt_options: Mutex::new(prompt_options),
        resource_options: applied_resources,
        resource_diagnostics: diagnostics,
    })
}

fn assemble_prompt(
    prompt: &mut BuildSystemPromptOptions,
    active_tools: &[String],
    cwd: &std::path::Path,
    registries: &pi_core::FrozenRegistries,
) -> Result<String, RuntimeError> {
    prompt.selected_tools = active_tools.to_vec();
    prompt.cwd = cwd.to_path_buf();
    prompt.tool_snippets.clear();
    prompt.prompt_guidelines.clear();
    for spec in registries
        .tool_specs(active_tools)
        .map_err(|error| RuntimeError::Build(error.to_string()))?
    {
        if let Some(snippet) = spec.prompt_snippet {
            prompt
                .tool_snippets
                .insert(spec.name.clone(), normalize_snippet(&snippet));
        }
        prompt.prompt_guidelines.extend(spec.prompt_guidelines);
    }
    Ok(build_system_prompt(prompt))
}

fn parse_command(input: &str) -> Option<(&str, &str)> {
    let command = input.strip_prefix('/')?;
    if command.is_empty() {
        return None;
    }
    let split = command.find(char::is_whitespace);
    Some(match split {
        Some(index) => (&command[..index], command[index..].trim()),
        None => (command, ""),
    })
}

fn normalize_snippet(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Clone)]
pub struct PiRuntime {
    agent: Agent,
    cwd: Arc<std::path::PathBuf>,
    blueprint: Arc<RuntimeBlueprint>,
    generation: Arc<RwLock<Arc<RuntimeGeneration>>>,
    reload_lock: Arc<tokio::sync::Mutex<()>>,
}

impl PiRuntime {
    pub fn builder() -> PiRuntimeBuilder {
        PiRuntimeBuilder::new()
    }

    pub fn agent(&self) -> &Agent {
        &self.agent
    }

    pub fn cwd(&self) -> &std::path::Path {
        &self.cwd
    }

    pub fn plugin_order(&self) -> Vec<PluginId> {
        self.agent.runtime().plugins().plugin_order()
    }

    pub fn provider_plugin_order(&self) -> Vec<PluginId> {
        self.current_generation().provider_plugins.plugin_order()
    }

    pub fn models(&self) -> Vec<ModelSpec> {
        self.agent.runtime().registries().model_specs()
    }

    pub fn available_models(&self) -> Vec<ModelSpec> {
        self.agent
            .runtime()
            .registries()
            .model_runtime()
            .available_models()
    }

    pub fn provider_statuses(&self) -> Vec<pi_core::ProviderStatus> {
        self.agent
            .runtime()
            .registries()
            .model_runtime()
            .provider_statuses()
    }

    pub fn model(&self, provider: &ProviderId, model: &ModelId) -> Option<ModelSpec> {
        self.agent
            .runtime()
            .registries()
            .model(provider, model)
            .cloned()
    }

    pub fn resolve_model_reference(
        &self,
        current_provider: &ProviderId,
        reference: &str,
    ) -> Option<ModelSpec> {
        self.agent
            .runtime()
            .registries()
            .model_runtime()
            .resolve_reference(current_provider, reference)
    }

    pub fn resolve_available_model_reference(
        &self,
        current_provider: &ProviderId,
        reference: &str,
    ) -> Option<ModelSpec> {
        self.agent
            .runtime()
            .registries()
            .model_runtime()
            .resolve_available_reference(current_provider, reference)
    }

    pub fn provider_is_available(&self, provider: &ProviderId) -> bool {
        self.agent
            .runtime()
            .registries()
            .provider(provider)
            .is_some_and(|provider| provider.availability().is_available())
    }

    pub fn has_provider(&self, provider: &ProviderId) -> bool {
        self.agent
            .runtime()
            .registries()
            .provider(provider)
            .is_some()
    }

    pub fn generation(&self) -> u64 {
        self.agent.runtime().generation()
    }

    pub fn resource_diagnostics(&self) -> Vec<ResourceDiagnostic> {
        self.current_generation().resource_diagnostics.clone()
    }

    pub fn prompt_options(&self) -> Option<BuildSystemPromptOptions> {
        self.current_generation()
            .prompt_options
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn resource_options(&self) -> Option<ResourceLoaderOptions> {
        self.current_generation().resource_options.clone()
    }

    pub fn command_specs(&self) -> Vec<CommandSpec> {
        self.agent.runtime().registries().command_specs()
    }

    pub fn tool_specs(&self) -> Vec<pi_core::ToolSpec> {
        self.agent.runtime().registries().all_tool_specs()
    }

    /// Runs input hooks directly. Full text submissions dispatch registered
    /// commands before entering this hook chain.
    pub async fn process_input(
        &self,
        input: impl Into<String>,
    ) -> Result<InputPatch, RuntimeError> {
        let _reload_guard = self.reload_lock.lock().await;
        let runtime = self.agent.runtime();
        let (_, signal) = AbortHandle::new();
        runtime
            .plugins()
            .input(self.cwd(), &signal, InputEvent { text: input.into() })
            .await
            .map_err(|error| RuntimeError::Input(error.to_string()))
    }

    /// Executes a registered slash command. Returns `None` for ordinary or unknown input.
    pub async fn execute_command(
        &self,
        input: &str,
    ) -> Result<Option<CommandOutcome>, RuntimeError> {
        let _reload_guard = self.reload_lock.lock().await;
        let Some((name, arguments)) = parse_command(input) else {
            return Ok(None);
        };
        let runtime = self.agent.runtime();
        let Some(command) = runtime.registries().command(name) else {
            return Ok(None);
        };
        let (_, signal) = AbortHandle::new();
        command
            .execute(
                CommandContext {
                    cwd: self.cwd().to_path_buf(),
                    abort_signal: signal,
                },
                arguments.to_string(),
            )
            .await
            .map(Some)
            .map_err(|error| RuntimeError::Command {
                command: name.to_string(),
                message: error.to_string(),
            })
    }

    pub fn active_tools(&self) -> Vec<String> {
        self.agent.state().active_tools
    }

    /// Atomically updates active tools and the Pi base prompt. The agent must be idle.
    pub fn set_active_tools(
        &self,
        tools: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<(), RuntimeError> {
        let _reload_guard = self
            .reload_lock
            .try_lock()
            .map_err(|_| RuntimeError::Busy)?;
        let mut unique = std::collections::HashSet::new();
        let tools = tools
            .into_iter()
            .map(Into::into)
            .filter(|tool| unique.insert(tool.clone()))
            .collect::<Vec<String>>();
        let unknown = tools
            .iter()
            .filter(|tool| self.agent.runtime().registries().tool(tool).is_none())
            .cloned()
            .collect::<Vec<_>>();
        if !unknown.is_empty() {
            return Err(RuntimeError::UnknownTools(unknown.join(", ")));
        }
        let generation = self.current_generation();
        let mut stored_prompt = generation
            .prompt_options
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut next_prompt = stored_prompt
            .clone()
            .ok_or(RuntimeError::FinalPromptIsStatic)?;
        let cwd = next_prompt.cwd.clone();
        let runtime = self.agent.runtime();
        let system_prompt = assemble_prompt(&mut next_prompt, &tools, &cwd, runtime.registries())?;
        self.agent
            .configure(AgentConfigurationPatch {
                active_tools: Some(tools),
                system_prompt: Some(system_prompt),
                ..AgentConfigurationPatch::default()
            })
            .map_err(|error| RuntimeError::Agent(error.to_string()))?;
        *stored_prompt = Some(next_prompt);
        Ok(())
    }

    pub fn set_model(
        &self,
        provider_id: ProviderId,
        model_id: ModelId,
    ) -> Result<(), RuntimeError> {
        let _reload_guard = self
            .reload_lock
            .try_lock()
            .map_err(|_| RuntimeError::Busy)?;
        let runtime = self.agent.runtime();
        let registries = runtime.registries();
        if registries.has_providers() && registries.provider(&provider_id).is_none() {
            return Err(RuntimeError::Provider(format!(
                "provider not found: {provider_id}"
            )));
        }
        self.agent
            .configure(AgentConfigurationPatch {
                provider_id: Some(provider_id),
                model_id: Some(model_id),
                ..AgentConfigurationPatch::default()
            })
            .map_err(|error| RuntimeError::Agent(error.to_string()))
    }

    pub fn set_thinking_level(&self, thinking_level: ThinkingLevel) -> Result<(), RuntimeError> {
        let _reload_guard = self
            .reload_lock
            .try_lock()
            .map_err(|_| RuntimeError::Busy)?;
        self.agent
            .configure(AgentConfigurationPatch {
                thinking_level: Some(thinking_level),
                ..AgentConfigurationPatch::default()
            })
            .map_err(|error| RuntimeError::Agent(error.to_string()))
    }

    /// Restores persisted transcript and configuration into the current
    /// generation. The caller supplies the runtime; plugin instances and
    /// resource discovery are intentionally rebuilt outside the session file.
    pub fn restore_state(&self, restored: RuntimeRestoreState) -> Result<(), RuntimeError> {
        let _reload_guard = self
            .reload_lock
            .try_lock()
            .map_err(|_| RuntimeError::Busy)?;
        let runtime = self.agent.runtime();
        let registries = runtime.registries();
        if registries.has_providers() && registries.provider(&restored.provider_id).is_none() {
            return Err(RuntimeError::Provider(format!(
                "provider not found: {}",
                restored.provider_id
            )));
        }
        let generation = self.current_generation();
        let mut stored_prompt = generation
            .prompt_options
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut next_prompt = stored_prompt.clone();
        let system_prompt = if let Some(prompt) = &mut next_prompt {
            let cwd = prompt.cwd.clone();
            Some(assemble_prompt(
                prompt,
                &restored.active_tools,
                &cwd,
                self.agent.runtime().registries(),
            )?)
        } else {
            None
        };
        self.agent
            .restore(AgentRestoreState {
                system_prompt,
                provider_id: restored.provider_id,
                model_id: restored.model_id,
                thinking_level: restored.thinking_level,
                active_tools: restored.active_tools,
                messages: restored.messages,
            })
            .map_err(|error| RuntimeError::Agent(error.to_string()))?;
        *stored_prompt = next_prompt;
        Ok(())
    }

    pub async fn prompt(
        &self,
        input: impl Into<PromptInput>,
    ) -> Result<AgentLoopOutcome, RuntimeError> {
        Ok(self.prompt_recorded(input).await?.outcome)
    }

    /// Runs command dispatch, input hooks, and the agent under one generation
    /// lease so a reload cannot split a single text submission across two
    /// generations.
    pub async fn submit_text(
        &self,
        text: impl Into<String>,
    ) -> Result<TextSubmissionOutcome, RuntimeError> {
        match self.prepare_text_submission(text).await? {
            PreparedTextSubmission::Handled => Ok(TextSubmissionOutcome::Handled),
            PreparedTextSubmission::Agent(prepared) => prepared
                .run()
                .await
                .map(Box::new)
                .map(TextSubmissionOutcome::Agent),
        }
    }

    /// Preprocesses a text submission while retaining the generation lease.
    /// Session orchestration uses the gap before [`PreparedRuntimePrompt::run`]
    /// to durably record an operation and provision its initial user message.
    pub async fn prepare_text_submission(
        &self,
        text: impl Into<String>,
    ) -> Result<PreparedTextSubmission, RuntimeError> {
        let reload_guard = Arc::clone(&self.reload_lock).lock_owned().await;
        let mut text = text.into();
        let display_text = text.clone();
        let runtime = self.agent.runtime();
        if let Some((name, arguments)) = parse_command(&text)
            && let Some(command) = runtime.registries().command(name)
        {
            let (_, signal) = AbortHandle::new();
            match command
                .execute(
                    CommandContext {
                        cwd: self.cwd().to_path_buf(),
                        abort_signal: signal,
                    },
                    arguments.to_string(),
                )
                .await
                .map_err(|error| RuntimeError::Command {
                    command: name.to_string(),
                    message: error.to_string(),
                })? {
                CommandOutcome::Handled => return Ok(PreparedTextSubmission::Handled),
                CommandOutcome::TransformInput(transformed) => text = transformed,
            }
        }
        text = match self.process_input_locked(&runtime, &text).await? {
            InputPatch::Handled => return Ok(PreparedTextSubmission::Handled),
            InputPatch::Transform(transformed) => transformed,
            InputPatch::Continue => text,
        };
        Ok(PreparedTextSubmission::Agent(PreparedRuntimePrompt {
            runtime: self.clone(),
            _reload_guard: reload_guard,
            generation: runtime.generation(),
            display_text,
            text,
        }))
    }

    /// Runs command dispatch and input hooks against the generation currently
    /// owned by an active agent run. This deliberately does not acquire the
    /// reload mutex: the active prepared submission already holds that lease.
    pub async fn process_queued_text(
        &self,
        text: impl Into<String>,
    ) -> Result<QueuedTextOutcome, RuntimeError> {
        let mut text = text.into();
        let display_text = text.clone();
        let runtime = self.agent.runtime();
        if let Some((name, arguments)) = parse_command(&text)
            && let Some(command) = runtime.registries().command(name)
        {
            let (_, signal) = AbortHandle::new();
            match command
                .execute(
                    CommandContext {
                        cwd: self.cwd().to_path_buf(),
                        abort_signal: signal,
                    },
                    arguments.to_string(),
                )
                .await
                .map_err(|error| RuntimeError::Command {
                    command: name.to_string(),
                    message: error.to_string(),
                })? {
                CommandOutcome::Handled => return Ok(QueuedTextOutcome::Handled),
                CommandOutcome::TransformInput(transformed) => text = transformed,
            }
        }
        text = match self.process_input_locked(&runtime, &text).await? {
            InputPatch::Handled => return Ok(QueuedTextOutcome::Handled),
            InputPatch::Transform(transformed) => transformed,
            InputPatch::Continue => text,
        };
        Ok(QueuedTextOutcome::Message {
            generation: runtime.generation(),
            display_text,
            text,
        })
    }

    pub async fn prompt_recorded(
        &self,
        input: impl Into<PromptInput>,
    ) -> Result<RuntimePromptOutcome, RuntimeError> {
        let _reload_guard = self.reload_lock.lock().await;
        self.prompt_recorded_locked(input.into()).await
    }

    /// Continues from the restored transcript and captures the same generation
    /// metadata as a normal prompt. This is used after overflow compaction.
    pub async fn continue_recorded(&self) -> Result<RuntimePromptOutcome, RuntimeError> {
        let _reload_guard = self.reload_lock.lock().await;
        let runtime = self.agent.runtime();
        let state = self.agent.state();
        let generation = self.current_generation();
        debug_assert_eq!(runtime.generation(), generation.agent.generation());
        let prompt_options = generation
            .prompt_options
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let outcome = self
            .agent
            .continue_run()
            .await
            .map_err(|error| RuntimeError::Agent(error.to_string()))?;
        Ok(RuntimePromptOutcome {
            generation: runtime.generation(),
            base_system_prompt: runtime.system_prompt().to_string(),
            active_tools: state.active_tools,
            prompt_options,
            outcome,
        })
    }

    /// Runs an isolated, tool-free provider completion without mutating the
    /// agent transcript or emitting agent lifecycle events.
    pub async fn complete(
        &self,
        request: RuntimeCompletionRequest,
        signal: AbortSignal,
    ) -> Result<AssistantMessage, RuntimeError> {
        let _reload_guard = self.reload_lock.lock().await;
        let runtime = self.agent.runtime();
        let state = self.agent.state();
        let provider = runtime
            .registries()
            .provider(&state.provider_id)
            .ok_or_else(|| {
                RuntimeError::Provider(format!("provider not found: {}", state.provider_id))
            })?;
        let call_context = ProviderCallContext::new(
            runtime.generation(),
            self.cwd().to_path_buf(),
            state.provider_id.clone(),
            state.model_id.clone(),
            Arc::clone(runtime.provider_plugins()),
        );
        let mut stream = provider
            .stream(
                ProviderRequest {
                    model: state.model_id,
                    system_prompt: request.system_prompt,
                    messages: request.messages,
                    tools: Vec::new(),
                    thinking_level: request.thinking_level,
                    max_output_tokens: request.max_output_tokens,
                    headers: Default::default(),
                    sampling_params: Default::default(),
                },
                call_context,
                signal.child(),
            )
            .await
            .map_err(|error| RuntimeError::Provider(error.to_string()))?;
        let mut assembler = StreamAssembler::new();
        loop {
            let item = tokio::select! {
                biased;
                () = signal.wait() => return Err(RuntimeError::Aborted),
                item = stream.next() => item,
            };
            let Some(item) = item else {
                break;
            };
            assembler
                .push(item.map_err(|error| RuntimeError::Provider(error.to_string()))?)
                .map_err(|error| RuntimeError::Assembly(error.to_string()))?;
        }
        assembler
            .finish()
            .map_err(|error| RuntimeError::Assembly(error.to_string()))
    }

    /// Rebuilds every factory-backed plugin and all derived registries and
    /// prompt state. The current generation is retained if preparation or
    /// compatibility validation fails.
    pub async fn reload(&self) -> Result<ReloadReport, RuntimeError> {
        let _reload_guard = self.reload_lock.lock().await;
        let previous = self.agent.runtime();
        let next_id = previous.generation().saturating_add(1);
        let state = self.agent.state();
        let next = Arc::new(build_generation(
            &self.blueprint,
            next_id,
            &state.active_tools,
        )?);
        if previous.registries().provider(&state.provider_id).is_some()
            && next
                .agent
                .registries()
                .provider(&state.provider_id)
                .is_none()
        {
            return Err(RuntimeError::Provider(format!(
                "reload would remove the active provider: {}",
                state.provider_id
            )));
        }
        self.agent
            .replace_runtime(Arc::clone(&next.agent))
            .await
            .map_err(|error| RuntimeError::Agent(error.to_string()))?;
        *self
            .generation
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::clone(&next);
        Ok(ReloadReport {
            previous_generation: previous.generation(),
            generation: next_id,
            plugin_order: next.agent.plugins().plugin_order(),
            provider_plugin_order: next.provider_plugins.plugin_order(),
            resource_diagnostics: next.resource_diagnostics.clone(),
        })
    }

    pub fn abort(&self) {
        self.agent.abort();
    }

    pub async fn wait_for_idle(&self) {
        self.agent.wait_for_idle().await;
    }

    fn current_generation(&self) -> Arc<RuntimeGeneration> {
        Arc::clone(
            &self
                .generation
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    async fn process_input_locked(
        &self,
        runtime: &AgentRuntime,
        text: &str,
    ) -> Result<InputPatch, RuntimeError> {
        let (_, signal) = AbortHandle::new();
        runtime
            .plugins()
            .input(
                self.cwd(),
                &signal,
                InputEvent {
                    text: text.to_string(),
                },
            )
            .await
            .map_err(|error| RuntimeError::Input(error.to_string()))
    }

    async fn prompt_recorded_locked(
        &self,
        input: PromptInput,
    ) -> Result<RuntimePromptOutcome, RuntimeError> {
        let runtime = self.agent.runtime();
        let state = self.agent.state();
        let generation = self.current_generation();
        debug_assert_eq!(runtime.generation(), generation.agent.generation());
        let prompt_options = generation
            .prompt_options
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let outcome = self
            .agent
            .prompt(input)
            .await
            .map_err(|error| RuntimeError::Agent(error.to_string()))?;
        Ok(RuntimePromptOutcome {
            generation: runtime.generation(),
            base_system_prompt: runtime.system_prompt().to_string(),
            active_tools: state.active_tools,
            prompt_options,
            outcome,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_agent::{AgentEventListener, AgentOptions};
    use pi_core::{
        AgentEndEvent, AgentEvent, AgentPlugin, AgentStartEvent, BeforeAgentStartEvent,
        BeforeAgentStartPatch, BeforeProviderRequestEvent, Command, CommandError, ContentBlock,
        ContextEvent, ContextPatch, InputContext, InputEvent, InputPatch, Message, MessageEndEvent,
        MessageStartEvent, MessageUpdateEvent, PluginContext, PluginError, PluginId, Provider,
        ProviderCallContext, ProviderError, ProviderPlugin, ProviderPluginContext,
        ProviderRegisterContext, ProviderStream, RegisterContext, ResponseMetadata, StopReason,
        StreamEvent, TextContent, ToolCall, ToolCallBlock, ToolCallEvent, ToolCallPatch,
        ToolExecutionEndEvent, ToolExecutionStartEvent, ToolExecutionUpdateEvent, ToolResultEvent,
        ToolResultPatch, TurnEndEvent, TurnStartEvent, Usage, UserMessage,
    };
    use pi_test_support::TestToolsPlugin;
    use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};
    use serde_json::json;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::Notify;

    struct DuplicatePlugin;

    #[async_trait::async_trait]
    impl AgentPlugin for DuplicatePlugin {
        fn id(&self) -> PluginId {
            PluginId::new("duplicate")
        }
    }

    struct IdOnlyPlugin(&'static str);

    #[async_trait::async_trait]
    impl AgentPlugin for IdOnlyPlugin {
        fn id(&self) -> PluginId {
            PluginId::new(self.0)
        }
    }

    #[test]
    fn pinned_and_factory_sources_preserve_builder_order() {
        let runtime = PiRuntime::builder()
            .agent_plugin(IdOnlyPlugin("first"))
            .agent_plugin_factory(|| IdOnlyPlugin("second"))
            .agent_plugin(IdOnlyPlugin("third"))
            .build()
            .unwrap();

        assert_eq!(
            runtime.plugin_order(),
            vec![
                PluginId::new("first"),
                PluginId::new("second"),
                PluginId::new("third"),
            ]
        );
    }

    #[test]
    fn duplicate_plugin_ids_fail_runtime_construction() {
        let error = match PiRuntime::builder()
            .agent_plugin(DuplicatePlugin)
            .agent_plugin(DuplicatePlugin)
            .build()
        {
            Ok(_) => panic!("duplicate plugin IDs must fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("duplicate plugin id"));
    }

    #[test]
    fn duplicate_provider_plugin_ids_fail_runtime_construction() {
        let error = match PiRuntime::builder()
            .provider_plugin(ScriptedProviderPlugin::scripted([]))
            .provider_plugin(ScriptedProviderPlugin::scripted([]))
            .build()
        {
            Ok(_) => panic!("duplicate provider plugin IDs must fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("duplicate provider plugin id"));
    }

    #[tokio::test]
    async fn reload_rebuilds_factory_backed_provider_plugins() {
        let builds = Arc::new(AtomicUsize::new(0));
        let builds_for_factory = Arc::clone(&builds);
        let runtime = PiRuntime::builder()
            .provider_plugin_factory(move || {
                builds_for_factory.fetch_add(1, Ordering::SeqCst);
                ScriptedProviderPlugin::scripted([])
            })
            .build()
            .unwrap();

        assert_eq!(builds.load(Ordering::SeqCst), 1);
        assert_eq!(
            runtime.provider_plugin_order(),
            vec![PluginId::new("scripted-provider")]
        );
        let report = runtime.reload().await.unwrap();
        assert_eq!(builds.load(Ordering::SeqCst), 2);
        assert_eq!(
            report.provider_plugin_order,
            vec![PluginId::new("scripted-provider")]
        );
    }

    struct GenerationCatalogPlugin {
        name: String,
    }

    struct HookedProvider {
        payloads: Arc<Mutex<Vec<serde_json::Value>>>,
    }

    #[async_trait::async_trait]
    impl Provider for HookedProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("hooked")
        }

        async fn stream(
            &self,
            request: ProviderRequest,
            context: ProviderCallContext,
            signal: AbortSignal,
        ) -> Result<ProviderStream, ProviderError> {
            let payload = context
                .before_provider_request(
                    &signal,
                    json!({
                        "model": request.model.as_str(),
                        "messages": request.messages.len()
                    }),
                )
                .await?;
            self.payloads
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(payload);
            Ok(Box::pin(futures::stream::iter([
                Ok(StreamEvent::Start {
                    metadata: ResponseMetadata::new(
                        ProviderId::new("hooked"),
                        request.model,
                        "fixture",
                        0,
                    ),
                }),
                Ok(StreamEvent::TextStart { content_index: 0 }),
                Ok(StreamEvent::TextDelta {
                    content_index: 0,
                    delta: "ok".to_string(),
                }),
                Ok(StreamEvent::TextEnd {
                    content_index: 0,
                    text_signature: None,
                }),
                Ok(StreamEvent::Done {
                    reason: StopReason::Stop,
                    usage: Usage::default(),
                }),
            ])))
        }
    }

    struct HookedProviderPlugin {
        provider: Arc<HookedProvider>,
    }

    #[async_trait::async_trait]
    impl ProviderPlugin for HookedProviderPlugin {
        fn id(&self) -> PluginId {
            PluginId::new("hook-fixture")
        }

        fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_core::Result<()> {
            context.register_provider(self.provider.clone())
        }

        async fn before_provider_request(
            &self,
            context: ProviderPluginContext,
            event: BeforeProviderRequestEvent,
        ) -> std::result::Result<Option<serde_json::Value>, PluginError> {
            let mut payload = event.payload;
            payload["hook_generation"] = json!(context.generation);
            Ok(Some(payload))
        }
    }

    impl ProviderPlugin for GenerationCatalogPlugin {
        fn id(&self) -> PluginId {
            PluginId::new("generation-models")
        }

        fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_core::Result<()> {
            context.register_model(ModelSpec::new(
                "scripted",
                "test",
                self.name.clone(),
                "scripted",
            ))
        }
    }

    #[tokio::test]
    async fn reload_rebuilds_model_catalog_provider_plugins_in_the_same_generation() {
        let builds = Arc::new(AtomicUsize::new(0));
        let builds_for_factory = Arc::clone(&builds);
        let runtime = PiRuntime::builder()
            .provider_plugin(ScriptedProviderPlugin::scripted([]))
            .provider_plugin_factory(move || GenerationCatalogPlugin {
                name: format!(
                    "generation-{}",
                    builds_for_factory.fetch_add(1, Ordering::SeqCst) + 1
                ),
            })
            .build()
            .unwrap();

        assert_eq!(runtime.models()[0].name, "generation-1");
        let report = runtime.reload().await.unwrap();
        assert_eq!(runtime.models()[0].name, "generation-2");
        assert_eq!(
            report.provider_plugin_order,
            vec![
                PluginId::new("scripted-provider"),
                PluginId::new("generation-models")
            ]
        );
    }

    #[tokio::test]
    async fn provider_plugin_request_hook_runs_through_the_agent_generation() {
        let payloads = Arc::new(Mutex::new(Vec::new()));
        let runtime = PiRuntime::builder()
            .provider_plugin(HookedProviderPlugin {
                provider: Arc::new(HookedProvider {
                    payloads: Arc::clone(&payloads),
                }),
            })
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("hooked"),
                model_id: ModelId::new("model"),
                ..AgentOptions::default()
            })
            .build()
            .unwrap();

        runtime.prompt("hello").await.unwrap();

        let payloads = payloads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0]["model"], "model");
        assert_eq!(payloads[0]["messages"], 1);
        assert_eq!(payloads[0]["hook_generation"], 1);
    }

    struct GenerationPlugin {
        value: usize,
        duplicate_command: bool,
    }

    #[async_trait::async_trait]
    impl AgentPlugin for GenerationPlugin {
        fn id(&self) -> PluginId {
            PluginId::new("generation")
        }

        fn register(&self, context: &mut RegisterContext<'_>) -> pi_core::Result<()> {
            context.register_command(Arc::new(GenerationCommand(self.value)))?;
            if self.duplicate_command {
                context.register_command(Arc::new(GenerationCommand(self.value)))?;
            }
            Ok(())
        }
    }

    struct GenerationCommand(usize);

    #[async_trait::async_trait]
    impl Command for GenerationCommand {
        fn spec(&self) -> CommandSpec {
            CommandSpec {
                name: "generation".to_string(),
                description: "Returns the runtime generation fixture".to_string(),
                argument_hint: None,
            }
        }

        async fn execute(
            &self,
            _context: CommandContext,
            _arguments: String,
        ) -> Result<CommandOutcome, CommandError> {
            Ok(CommandOutcome::TransformInput(self.0.to_string()))
        }
    }

    async fn generation_command(runtime: &PiRuntime) -> String {
        let Some(CommandOutcome::TransformInput(value)) =
            runtime.execute_command("/generation").await.unwrap()
        else {
            panic!("expected generation command transformation")
        };
        value
    }

    struct SuffixInputPlugin;

    #[async_trait::async_trait]
    impl AgentPlugin for SuffixInputPlugin {
        fn id(&self) -> PluginId {
            PluginId::new("suffix-input")
        }

        async fn input(
            &self,
            _context: InputContext,
            event: InputEvent,
        ) -> Result<InputPatch, PluginError> {
            Ok(InputPatch::Transform(format!("{}|input", event.text)))
        }
    }

    #[tokio::test]
    async fn command_transforms_continue_through_input_hooks() {
        let scripted = ScriptedProviderPlugin::scripted([ScriptedTurn::Text("done".to_string())]);
        let provider = scripted.provider();
        let runtime = PiRuntime::builder()
            .agent_plugin(GenerationPlugin {
                value: 7,
                duplicate_command: false,
            })
            .agent_plugin(SuffixInputPlugin)
            .provider_plugin(scripted)
            .build()
            .unwrap();

        let outcome = runtime.submit_text("/generation").await.unwrap();
        assert!(matches!(outcome, TextSubmissionOutcome::Agent(_)));
        let requests = provider.requests();
        assert!(matches!(
            &requests[0].messages[0],
            Message::User(user)
                if matches!(&user.content[0], ContentBlock::Text(text)
                    if text.text == "7|input")
        ));
    }

    #[tokio::test]
    async fn prepared_command_retains_the_original_input_for_product_presentation() {
        let runtime = PiRuntime::builder()
            .agent_plugin(GenerationPlugin {
                value: 7,
                duplicate_command: false,
            })
            .agent_plugin(SuffixInputPlugin)
            .provider_plugin(ScriptedProviderPlugin::scripted([]))
            .build()
            .unwrap();

        let PreparedTextSubmission::Agent(prepared) = runtime
            .prepare_text_submission("/generation focus")
            .await
            .unwrap()
        else {
            panic!("expected an agent submission");
        };

        assert_eq!(prepared.display_text(), "/generation focus");
        assert_eq!(prepared.text(), "7|input");
    }

    #[tokio::test]
    async fn queued_command_retains_the_original_input_for_product_presentation() {
        let runtime = PiRuntime::builder()
            .agent_plugin(GenerationPlugin {
                value: 7,
                duplicate_command: false,
            })
            .agent_plugin(SuffixInputPlugin)
            .provider_plugin(ScriptedProviderPlugin::scripted([]))
            .build()
            .unwrap();

        let QueuedTextOutcome::Message {
            display_text, text, ..
        } = runtime
            .process_queued_text("/generation focus")
            .await
            .unwrap()
        else {
            panic!("expected a queued message");
        };

        assert_eq!(display_text, "/generation focus");
        assert_eq!(text, "7|input");
    }

    #[tokio::test]
    async fn reload_rebuilds_factory_backed_plugins_as_one_generation() {
        let builds = Arc::new(AtomicUsize::new(0));
        let builds_for_factory = Arc::clone(&builds);
        let runtime = PiRuntime::builder()
            .provider_plugin(ScriptedProviderPlugin::scripted([]))
            .agent_plugin_factory(move || GenerationPlugin {
                value: builds_for_factory.fetch_add(1, Ordering::SeqCst) + 1,
                duplicate_command: false,
            })
            .build()
            .unwrap();

        assert_eq!(runtime.generation(), 1);
        assert_eq!(generation_command(&runtime).await, "1");
        let report = runtime.reload().await.unwrap();
        assert_eq!(report.previous_generation, 1);
        assert_eq!(report.generation, 2);
        assert_eq!(runtime.generation(), 2);
        assert_eq!(generation_command(&runtime).await, "2");
    }

    #[tokio::test]
    async fn failed_reload_keeps_the_previous_generation_intact() {
        let builds = Arc::new(AtomicUsize::new(0));
        let builds_for_factory = Arc::clone(&builds);
        let runtime = PiRuntime::builder()
            .provider_plugin(ScriptedProviderPlugin::scripted([]))
            .agent_plugin_factory(move || {
                let value = builds_for_factory.fetch_add(1, Ordering::SeqCst) + 1;
                GenerationPlugin {
                    value,
                    duplicate_command: value > 1,
                }
            })
            .build()
            .unwrap();

        assert_eq!(generation_command(&runtime).await, "1");
        let error = runtime.reload().await.unwrap_err();
        assert!(error.to_string().contains("duplicate command"));
        assert_eq!(runtime.generation(), 1);
        assert_eq!(generation_command(&runtime).await, "1");
    }

    #[tokio::test]
    async fn factory_load_failure_keeps_the_previous_generation_intact() {
        let builds = Arc::new(AtomicUsize::new(0));
        let builds_for_factory = Arc::clone(&builds);
        let runtime = PiRuntime::builder()
            .provider_plugin(ScriptedProviderPlugin::scripted([]))
            .try_agent_plugin_factory(move || {
                let value = builds_for_factory.fetch_add(1, Ordering::SeqCst) + 1;
                if value > 1 {
                    Err("fixture load failed")
                } else {
                    Ok(GenerationPlugin {
                        value,
                        duplicate_command: false,
                    })
                }
            })
            .build()
            .unwrap();

        assert_eq!(generation_command(&runtime).await, "1");
        let error = runtime.reload().await.unwrap_err();
        assert!(error.to_string().contains("fixture load failed"));
        assert_eq!(runtime.generation(), 1);
        assert_eq!(generation_command(&runtime).await, "1");
    }

    struct GenerationLeasePlugin {
        value: usize,
        entered: Arc<Notify>,
        release: Arc<Notify>,
    }

    #[async_trait::async_trait]
    impl AgentPlugin for GenerationLeasePlugin {
        fn id(&self) -> PluginId {
            PluginId::new("generation-lease")
        }

        async fn input(
            &self,
            _context: InputContext,
            event: InputEvent,
        ) -> Result<InputPatch, PluginError> {
            if event.text != "lease" {
                return Ok(InputPatch::Continue);
            }
            self.entered.notify_one();
            self.release.notified().await;
            Ok(InputPatch::Transform(format!("input-{}", self.value)))
        }

        async fn before_agent_start(
            &self,
            _context: PluginContext,
            event: BeforeAgentStartEvent,
        ) -> Result<BeforeAgentStartPatch, PluginError> {
            Ok(BeforeAgentStartPatch {
                system_prompt: Some(format!("{}|hook-{}", event.system_prompt, self.value)),
                messages: Vec::new(),
            })
        }
    }

    #[tokio::test]
    async fn text_submission_holds_one_generation_across_input_and_prompt() {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let builds = Arc::new(AtomicUsize::new(0));
        let scripted = ScriptedProviderPlugin::scripted([ScriptedTurn::Text("done".to_string())]);
        let provider = scripted.provider();
        let runtime = PiRuntime::builder()
            .provider_plugin(scripted)
            .agent_plugin_factory({
                let entered = Arc::clone(&entered);
                let release = Arc::clone(&release);
                move || GenerationLeasePlugin {
                    value: builds.fetch_add(1, Ordering::SeqCst) + 1,
                    entered: Arc::clone(&entered),
                    release: Arc::clone(&release),
                }
            })
            .build()
            .unwrap();

        let submit = {
            let runtime = runtime.clone();
            tokio::spawn(async move { runtime.submit_text("lease").await.unwrap() })
        };
        entered.notified().await;
        let reload = {
            let runtime = runtime.clone();
            tokio::spawn(async move { runtime.reload().await.unwrap() })
        };
        tokio::task::yield_now().await;
        assert_eq!(runtime.generation(), 1);

        release.notify_one();
        let TextSubmissionOutcome::Agent(recorded) = submit.await.unwrap() else {
            panic!("expected agent run")
        };
        assert_eq!(recorded.generation, 1);
        assert_eq!(reload.await.unwrap().generation, 2);

        let requests = provider.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].system_prompt.ends_with("|hook-1"));
        assert!(matches!(&requests[0].messages[0], Message::User(message)
            if matches!(&message.content[0], ContentBlock::Text(text) if text.text == "input-1")));
    }

    struct PromptHookPlugin {
        id: &'static str,
        suffix: &'static str,
        inject: Option<&'static str>,
    }

    #[async_trait::async_trait]
    impl AgentPlugin for PromptHookPlugin {
        fn id(&self) -> PluginId {
            PluginId::new(self.id)
        }

        async fn before_agent_start(
            &self,
            _context: PluginContext,
            event: BeforeAgentStartEvent,
        ) -> Result<BeforeAgentStartPatch, PluginError> {
            Ok(BeforeAgentStartPatch {
                system_prompt: Some(format!("{}{}", event.system_prompt, self.suffix)),
                messages: self
                    .inject
                    .map(|text| vec![Message::User(UserMessage::text(text, 1))])
                    .unwrap_or_default(),
            })
        }
    }

    struct FailingPromptHook;

    #[async_trait::async_trait]
    impl AgentPlugin for FailingPromptHook {
        fn id(&self) -> PluginId {
            PluginId::new("failing-prompt")
        }

        async fn before_agent_start(
            &self,
            _context: PluginContext,
            _event: BeforeAgentStartEvent,
        ) -> Result<BeforeAgentStartPatch, PluginError> {
            Err(PluginError::Registration(
                "intentional prompt hook failure".to_string(),
            ))
        }
    }

    #[tokio::test]
    async fn before_agent_start_failure_restores_idle_without_mutating_transcript() {
        let runtime = PiRuntime::builder()
            .agent_plugin(FailingPromptHook)
            .provider_plugin(ScriptedProviderPlugin::scripted([ScriptedTurn::Text(
                "unused".to_string(),
            )]))
            .build()
            .unwrap();
        let error = runtime.prompt("hello").await.unwrap_err();
        assert!(error.to_string().contains("before_agent_start"));
        let state = runtime.agent().state();
        assert!(!state.is_running);
        assert!(state.messages.is_empty());
    }

    #[tokio::test]
    async fn before_agent_start_chains_prompt_per_run_without_mutating_base() {
        let scripted = ScriptedProviderPlugin::scripted([
            ScriptedTurn::Text("one".to_string()),
            ScriptedTurn::Text("two".to_string()),
        ]);
        let provider = scripted.provider();
        let runtime = PiRuntime::builder()
            .agent_plugin(PromptHookPlugin {
                id: "prompt-a",
                suffix: "|a",
                inject: Some("injected"),
            })
            .agent_plugin(PromptHookPlugin {
                id: "prompt-b",
                suffix: "|b",
                inject: None,
            })
            .provider_plugin(scripted)
            .agent_options(AgentOptions {
                system_prompt: "base".to_string(),
                ..AgentOptions::default()
            })
            .build()
            .unwrap();

        runtime.prompt("first").await.unwrap();
        runtime.prompt("second").await.unwrap();

        let requests = provider.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].system_prompt, "base|a|b");
        assert_eq!(requests[1].system_prompt, "base|a|b");
        assert!(matches!(&requests[0].messages[0], Message::User(message)
            if matches!(&message.content[0], ContentBlock::Text(text) if text.text == "injected")));
        assert_eq!(runtime.agent().state().system_prompt, "base");
    }

    #[derive(Clone)]
    struct LifecyclePlugin {
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl LifecyclePlugin {
        fn push(&self, event: &'static str) {
            self.events.lock().unwrap().push(event);
        }
    }

    #[async_trait::async_trait]
    impl AgentPlugin for LifecyclePlugin {
        fn id(&self) -> PluginId {
            PluginId::new("lifecycle")
        }

        async fn agent_start(
            &self,
            _: PluginContext,
            _: AgentStartEvent,
        ) -> Result<(), PluginError> {
            self.push("agent_start");
            Ok(())
        }
        async fn agent_end(&self, _: PluginContext, _: AgentEndEvent) -> Result<(), PluginError> {
            self.push("agent_end");
            Ok(())
        }
        async fn turn_start(&self, _: PluginContext, _: TurnStartEvent) -> Result<(), PluginError> {
            self.push("turn_start");
            Ok(())
        }
        async fn turn_end(&self, _: PluginContext, _: TurnEndEvent) -> Result<(), PluginError> {
            self.push("turn_end");
            Ok(())
        }
        async fn message_start(
            &self,
            _: PluginContext,
            _: MessageStartEvent,
        ) -> Result<(), PluginError> {
            self.push("message_start");
            Ok(())
        }
        async fn message_update(
            &self,
            _: PluginContext,
            _: MessageUpdateEvent,
        ) -> Result<(), PluginError> {
            self.push("message_update");
            Ok(())
        }
        async fn message_end(
            &self,
            _: PluginContext,
            _: MessageEndEvent,
        ) -> Result<(), PluginError> {
            self.push("message_end");
            Ok(())
        }
        async fn tool_execution_start(
            &self,
            _: PluginContext,
            _: ToolExecutionStartEvent,
        ) -> Result<(), PluginError> {
            self.push("tool_execution_start");
            Ok(())
        }
        async fn tool_execution_update(
            &self,
            _: PluginContext,
            _: ToolExecutionUpdateEvent,
        ) -> Result<(), PluginError> {
            self.push("tool_execution_update");
            Ok(())
        }
        async fn tool_execution_end(
            &self,
            _: PluginContext,
            _: ToolExecutionEndEvent,
        ) -> Result<(), PluginError> {
            self.push("tool_execution_end");
            Ok(())
        }
    }

    struct ContextToolHookPlugin;

    #[async_trait::async_trait]
    impl AgentPlugin for ContextToolHookPlugin {
        fn id(&self) -> PluginId {
            PluginId::new("context-tool-hooks")
        }

        async fn context(
            &self,
            _context: PluginContext,
            mut event: ContextEvent,
        ) -> Result<ContextPatch, PluginError> {
            event
                .messages
                .insert(0, Message::User(UserMessage::text("context-only", 1)));
            Ok(ContextPatch {
                messages: Some(event.messages),
            })
        }

        async fn tool_call(
            &self,
            _context: PluginContext,
            _event: ToolCallEvent,
        ) -> Result<ToolCallPatch, PluginError> {
            Ok(ToolCallPatch {
                arguments: Some(json!({"text":"patched-arg"})),
                block: None,
            })
        }

        async fn tool_result(
            &self,
            _context: PluginContext,
            _event: ToolResultEvent,
        ) -> Result<ToolResultPatch, PluginError> {
            Ok(ToolResultPatch {
                content: Some(vec![ContentBlock::Text(TextContent::new("patched-result"))]),
                ..ToolResultPatch::default()
            })
        }
    }

    #[tokio::test]
    async fn plugin_lifecycle_context_tool_call_and_tool_result_hooks_work() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let scripted = ScriptedProviderPlugin::scripted([
            ScriptedTurn::ToolCalls(vec![ToolCall::new(
                "echo-1",
                "echo",
                json!({"text":"original"}),
            )]),
            ScriptedTurn::Text("done".to_string()),
        ]);
        let provider = scripted.provider();
        let runtime = PiRuntime::builder()
            .agent_plugin(LifecyclePlugin {
                events: Arc::clone(&captured),
            })
            .agent_plugin(ContextToolHookPlugin)
            .agent_plugin(TestToolsPlugin::new())
            .provider_plugin(scripted)
            .agent_options(AgentOptions {
                active_tools: vec!["echo".to_string()],
                ..AgentOptions::default()
            })
            .build()
            .unwrap();
        let outcome = runtime.prompt("run").await.unwrap();
        let requests = provider.requests();
        assert!(matches!(&requests[0].messages[0], Message::User(message)
            if matches!(&message.content[0], ContentBlock::Text(text) if text.text == "context-only")));
        assert!(!outcome.new_messages.iter().any(|message| matches!(message, Message::User(user)
            if matches!(&user.content[0], ContentBlock::Text(text) if text.text == "context-only"))));
        let result = outcome
            .new_messages
            .iter()
            .find_map(|message| match message {
                Message::ToolResult(result) => Some(result),
                _ => None,
            })
            .unwrap();
        assert!(
            matches!(&result.content[0], ContentBlock::Text(text) if text.text == "patched-result")
        );
        let events = captured.lock().unwrap();
        for expected in [
            "agent_start",
            "turn_start",
            "message_start",
            "message_update",
            "tool_execution_start",
            "tool_execution_end",
            "turn_end",
            "agent_end",
        ] {
            assert!(events.contains(&expected), "missing {expected}: {events:?}");
        }
    }

    #[tokio::test]
    async fn full_plugin_first_tool_loop_preserves_result_order() {
        let test_tools = TestToolsPlugin::new();
        let scripted_plugin = ScriptedProviderPlugin::scripted([
            ScriptedTurn::ToolCalls(vec![
                ToolCall::new("call-1", "delay", json!({"value": "one", "delayMs": 80})),
                ToolCall::new("call-2", "delay", json!({"value": "two", "delayMs": 10})),
            ]),
            ScriptedTurn::Text("done".to_string()),
        ]);
        let provider = scripted_plugin.provider();
        let runtime = PiRuntime::builder()
            .agent_plugin(test_tools.clone())
            .provider_plugin(scripted_plugin)
            .agent_options(AgentOptions {
                active_tools: vec!["delay".to_string()],
                ..AgentOptions::default()
            })
            .build()
            .unwrap();

        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&events);
        runtime.agent().subscribe(Arc::new(
            move |event: AgentEvent, _signal: pi_core::AbortSignal| {
                let captured = Arc::clone(&captured);
                async move {
                    captured
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(event);
                    Ok(())
                }
            },
        ));

        let outcome = runtime.prompt("run").await.unwrap();
        assert_eq!(test_tools.completions(), vec!["two", "one"]);

        let end_order = events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter_map(|event| match event {
                AgentEvent::ToolExecutionEnd { tool_call_id, .. } => Some(tool_call_id.to_string()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(end_order, vec!["call-2", "call-1"]);

        let tool_result_order = outcome
            .new_messages
            .iter()
            .filter_map(|message| match message {
                Message::ToolResult(result) => Some(result.tool_call_id.to_string()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(tool_result_order, vec!["call-1", "call-2"]);

        let requests = provider.requests();
        assert_eq!(requests.len(), 2);
        let second_request_results = requests[1]
            .messages
            .iter()
            .filter_map(|message| match message {
                Message::ToolResult(result) => Some(result.tool_call_id.to_string()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(second_request_results, vec!["call-1", "call-2"]);
        assert!(!runtime.agent().state().is_running);
    }

    #[tokio::test]
    async fn max_tool_iterations_stops_with_balanced_lifecycle() {
        let runtime = PiRuntime::builder()
            .agent_plugin(TestToolsPlugin::new())
            .provider_plugin(ScriptedProviderPlugin::scripted([
                ScriptedTurn::ToolCalls(vec![ToolCall::new(
                    "echo-1",
                    "echo",
                    json!({"text": "one"}),
                )]),
                ScriptedTurn::ToolCalls(vec![ToolCall::new(
                    "echo-2",
                    "echo",
                    json!({"text": "two"}),
                )]),
            ]))
            .agent_options(AgentOptions {
                active_tools: vec!["echo".to_string()],
                max_tool_iterations: 1,
                ..AgentOptions::default()
            })
            .build()
            .unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&events);
        runtime.agent().subscribe(Arc::new(
            move |event: AgentEvent, _signal: pi_core::AbortSignal| {
                let captured = Arc::clone(&captured);
                async move {
                    captured
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(event);
                    Ok(())
                }
            },
        ));

        let outcome = runtime.prompt("loop").await.unwrap();
        assert_eq!(outcome.stop, pi_agent::AgentLoopStop::MaxToolIterations);
        assert!(
            outcome.new_messages.iter().any(|message| {
                matches!(message, Message::ToolResult(result)
                    if result.tool_call_id.as_str() == "echo-2" && result.is_error)
            }),
            "the tool call that crosses the iteration limit must receive an error result"
        );
        let events = events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, AgentEvent::TurnStart))
                .count(),
            2
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, AgentEvent::TurnEnd { .. }))
                .count(),
            2
        );
        assert!(matches!(events.last(), Some(AgentEvent::AgentEnd { .. })));
    }

    #[tokio::test]
    async fn sequential_tool_forces_source_order_execution() {
        let test_tools = TestToolsPlugin::new();
        let runtime = PiRuntime::builder()
            .agent_plugin(test_tools.clone())
            .provider_plugin(ScriptedProviderPlugin::scripted([
                ScriptedTurn::ToolCalls(vec![
                    ToolCall::new(
                        "call-1",
                        "sequential_delay",
                        json!({"value": "one", "delayMs": 60}),
                    ),
                    ToolCall::new("call-2", "delay", json!({"value": "two", "delayMs": 1})),
                ]),
                ScriptedTurn::Text("done".to_string()),
            ]))
            .agent_options(AgentOptions {
                active_tools: vec!["sequential_delay".to_string(), "delay".to_string()],
                ..AgentOptions::default()
            })
            .build()
            .unwrap();

        runtime.prompt("run sequentially").await.unwrap();
        assert_eq!(test_tools.completions(), vec!["one", "two"]);
    }

    #[tokio::test]
    async fn malformed_provider_stream_finishes_with_error_lifecycle() {
        let runtime = PiRuntime::builder()
            .provider_plugin(ScriptedProviderPlugin::scripted([ScriptedTurn::Events(
                vec![pi_core::StreamEvent::TextDelta {
                    content_index: 0,
                    delta: "invalid".to_string(),
                }],
            )]))
            .build()
            .unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&events);
        runtime.agent().subscribe(Arc::new(
            move |event: AgentEvent, _signal: pi_core::AbortSignal| {
                let captured = Arc::clone(&captured);
                async move {
                    captured
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(event);
                    Ok(())
                }
            },
        ));

        let outcome = runtime.prompt("malformed").await.unwrap();
        assert_eq!(outcome.stop, pi_agent::AgentLoopStop::ProviderError);
        let events = events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::MessageEnd {
                message: Message::Assistant(message),
            } if message.stop_reason == StopReason::Error
        )));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, AgentEvent::TurnEnd { .. }))
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, AgentEvent::AgentEnd { .. }))
        );
    }

    #[tokio::test]
    async fn unknown_and_failed_tools_become_error_results() {
        let runtime = PiRuntime::builder()
            .agent_plugin(TestToolsPlugin::new())
            .provider_plugin(ScriptedProviderPlugin::scripted([
                ScriptedTurn::ToolCalls(vec![
                    ToolCall::new("missing-1", "missing", json!({})),
                    ToolCall::new("fail-1", "fail", json!({})),
                ]),
                ScriptedTurn::Text("recovered".to_string()),
            ]))
            .agent_options(AgentOptions {
                active_tools: vec!["fail".to_string()],
                ..AgentOptions::default()
            })
            .build()
            .unwrap();

        let outcome = runtime.prompt("fail safely").await.unwrap();
        let results = outcome
            .new_messages
            .iter()
            .filter_map(|message| match message {
                Message::ToolResult(result) => Some(result),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|result| result.is_error));
        assert_eq!(results[0].tool_call_id.as_str(), "missing-1");
        assert_eq!(results[1].tool_call_id.as_str(), "fail-1");
    }

    #[tokio::test]
    async fn aborting_provider_wait_emits_aborted_message_and_settles() {
        let runtime = PiRuntime::builder()
            .provider_plugin(ScriptedProviderPlugin::scripted([
                ScriptedTurn::WaitForAbort,
            ]))
            .build()
            .unwrap();
        let runner = runtime.clone();
        let task = tokio::spawn(async move { runner.prompt("wait").await });
        tokio::time::timeout(Duration::from_secs(1), async {
            while !runtime.agent().state().is_running {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        runtime.abort();
        let outcome = task.await.unwrap().unwrap();
        assert_eq!(outcome.stop, pi_agent::AgentLoopStop::Aborted);
        assert!(!runtime.agent().state().is_running);
        let last = outcome.new_messages.last().unwrap();
        assert!(matches!(
            last,
            Message::Assistant(message) if message.stop_reason == StopReason::Aborted
        ));
    }

    struct SettlingListener {
        started: Arc<AtomicBool>,
        settled: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl AgentEventListener for SettlingListener {
        async fn on_event(
            &self,
            event: AgentEvent,
            _signal: pi_core::AbortSignal,
        ) -> Result<(), pi_agent::EventError> {
            if matches!(event, AgentEvent::AgentEnd { .. }) {
                self.started.store(true, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(40)).await;
                self.settled.store(true, Ordering::SeqCst);
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn agent_remains_running_until_agent_end_listener_settles() {
        let runtime = PiRuntime::builder()
            .provider_plugin(ScriptedProviderPlugin::scripted([ScriptedTurn::Text(
                "done".to_string(),
            )]))
            .build()
            .unwrap();
        let started = Arc::new(AtomicBool::new(false));
        let settled = Arc::new(AtomicBool::new(false));
        runtime.agent().subscribe(Arc::new(SettlingListener {
            started: Arc::clone(&started),
            settled: Arc::clone(&settled),
        }));
        let runner = runtime.clone();
        let task = tokio::spawn(async move { runner.prompt("settle").await });

        tokio::time::timeout(Duration::from_secs(1), async {
            while !started.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(runtime.agent().state().is_running);
        assert!(!settled.load(Ordering::SeqCst));

        task.await.unwrap().unwrap();
        assert!(settled.load(Ordering::SeqCst));
        assert!(!runtime.agent().state().is_running);
    }

    struct BlockEchoPlugin;

    #[async_trait::async_trait]
    impl AgentPlugin for BlockEchoPlugin {
        fn id(&self) -> PluginId {
            PluginId::new("block-echo")
        }

        async fn tool_call(
            &self,
            _context: PluginContext,
            event: ToolCallEvent,
        ) -> Result<ToolCallPatch, PluginError> {
            Ok(if event.tool_call.name == "echo" {
                ToolCallPatch {
                    arguments: None,
                    block: Some(ToolCallBlock {
                        reason: "blocked by test".to_string(),
                        terminate: false,
                    }),
                }
            } else {
                ToolCallPatch::default()
            })
        }
    }

    struct PatchToolResultPlugin;

    #[async_trait::async_trait]
    impl AgentPlugin for PatchToolResultPlugin {
        fn id(&self) -> PluginId {
            PluginId::new("patch-tool-result")
        }

        async fn tool_result(
            &self,
            _context: PluginContext,
            _event: ToolResultEvent,
        ) -> Result<ToolResultPatch, PluginError> {
            Ok(ToolResultPatch {
                content: Some(vec![ContentBlock::Text(TextContent::new("patched"))]),
                ..ToolResultPatch::default()
            })
        }
    }

    #[tokio::test]
    async fn plugin_can_block_tool_and_blocked_call_skips_after_hook() {
        let runtime = PiRuntime::builder()
            .agent_plugin(TestToolsPlugin::new())
            .agent_plugin(BlockEchoPlugin)
            .agent_plugin(PatchToolResultPlugin)
            .provider_plugin(ScriptedProviderPlugin::scripted([
                ScriptedTurn::ToolCalls(vec![ToolCall::new(
                    "echo-1",
                    "echo",
                    json!({"text": "original"}),
                )]),
                ScriptedTurn::Text("done".to_string()),
            ]))
            .agent_options(AgentOptions {
                active_tools: vec!["echo".to_string()],
                ..AgentOptions::default()
            })
            .build()
            .unwrap();

        let outcome = runtime.prompt("block").await.unwrap();
        let result = outcome
            .new_messages
            .iter()
            .find_map(|message| match message {
                Message::ToolResult(result) => Some(result),
                _ => None,
            })
            .unwrap();
        let text = result.content.iter().find_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        });
        assert_eq!(text, Some("blocked by test"));
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn tool_result_hook_patches_executed_result() {
        let runtime = PiRuntime::builder()
            .agent_plugin(TestToolsPlugin::new())
            .agent_plugin(PatchToolResultPlugin)
            .provider_plugin(ScriptedProviderPlugin::scripted([
                ScriptedTurn::ToolCalls(vec![ToolCall::new(
                    "echo-1",
                    "echo",
                    json!({"text": "original"}),
                )]),
                ScriptedTurn::Text("done".to_string()),
            ]))
            .agent_options(AgentOptions {
                active_tools: vec!["echo".to_string()],
                ..AgentOptions::default()
            })
            .build()
            .unwrap();

        let outcome = runtime.prompt("patch").await.unwrap();
        let result = outcome
            .new_messages
            .iter()
            .find_map(|message| match message {
                Message::ToolResult(result) => Some(result),
                _ => None,
            })
            .unwrap();
        let text = result.content.iter().find_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        });
        assert_eq!(text, Some("patched"));
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn queued_tool_update_precedes_execution_end() {
        let runtime = PiRuntime::builder()
            .agent_plugin(TestToolsPlugin::new())
            .provider_plugin(ScriptedProviderPlugin::scripted([
                ScriptedTurn::ToolCalls(vec![ToolCall::new("update-1", "update", json!({}))]),
                ScriptedTurn::Text("done".to_string()),
            ]))
            .agent_options(AgentOptions {
                active_tools: vec!["update".to_string()],
                ..AgentOptions::default()
            })
            .build()
            .unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&events);
        runtime.agent().subscribe(Arc::new(
            move |event: AgentEvent, _signal: pi_core::AbortSignal| {
                let captured = Arc::clone(&captured);
                async move {
                    captured
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(event);
                    Ok(())
                }
            },
        ));

        runtime.prompt("update").await.unwrap();
        let events = events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let update_index = events
            .iter()
            .position(|event| matches!(event, AgentEvent::ToolExecutionUpdate { .. }))
            .unwrap();
        let end_index = events
            .iter()
            .position(|event| matches!(event, AgentEvent::ToolExecutionEnd { .. }))
            .unwrap();
        assert!(update_index < end_index);
    }

    #[tokio::test]
    async fn aborting_running_tool_settles_batch() {
        let runtime = PiRuntime::builder()
            .agent_plugin(TestToolsPlugin::new())
            .provider_plugin(ScriptedProviderPlugin::scripted([
                ScriptedTurn::ToolCalls(vec![ToolCall::new("wait-1", "wait_for_abort", json!({}))]),
                ScriptedTurn::Text("must not be requested".to_string()),
            ]))
            .agent_options(AgentOptions {
                active_tools: vec!["wait_for_abort".to_string()],
                ..AgentOptions::default()
            })
            .build()
            .unwrap();
        let started = Arc::new(AtomicBool::new(false));
        let started_for_listener = Arc::clone(&started);
        runtime.agent().subscribe(Arc::new(
            move |event: AgentEvent, _signal: pi_core::AbortSignal| {
                let started = Arc::clone(&started_for_listener);
                async move {
                    if matches!(event, AgentEvent::ToolExecutionStart { .. }) {
                        started.store(true, Ordering::SeqCst);
                    }
                    Ok(())
                }
            },
        ));
        let runner = runtime.clone();
        let task = tokio::spawn(async move { runner.prompt("wait").await });
        tokio::time::timeout(Duration::from_secs(1), async {
            while !started.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        runtime.abort();
        let outcome = task.await.unwrap().unwrap();
        assert!(!runtime.agent().state().is_running);
        assert!(
            outcome.new_messages.iter().any(|message| {
                matches!(message, Message::ToolResult(result) if result.is_error)
            })
        );
    }

    #[tokio::test]
    async fn follow_up_after_text_turn_keeps_run_alive() {
        let runtime = PiRuntime::builder()
            .provider_plugin(ScriptedProviderPlugin::scripted([
                ScriptedTurn::Text("first".to_string()),
                ScriptedTurn::Text("second".to_string()),
            ]))
            .build()
            .unwrap();
        let queued = Arc::new(AtomicBool::new(false));
        let agent = runtime.agent().clone();
        let queued_for_listener = Arc::clone(&queued);
        runtime.agent().subscribe(Arc::new(
            move |event: AgentEvent, _signal: pi_core::AbortSignal| {
                let agent = agent.clone();
                let queued = Arc::clone(&queued_for_listener);
                async move {
                    if matches!(event, AgentEvent::TurnEnd { .. })
                        && !queued.swap(true, Ordering::SeqCst)
                    {
                        agent.follow_up(Message::User(UserMessage::text("follow", 1)));
                    }
                    Ok(())
                }
            },
        ));

        let outcome = runtime.prompt("start").await.unwrap();
        assert_eq!(
            assistant_texts(&outcome.new_messages),
            vec!["first", "second"]
        );
    }

    #[tokio::test]
    async fn steering_after_text_turn_keeps_run_alive() {
        let runtime = PiRuntime::builder()
            .provider_plugin(ScriptedProviderPlugin::scripted([
                ScriptedTurn::Text("first".to_string()),
                ScriptedTurn::Text("second".to_string()),
            ]))
            .build()
            .unwrap();
        let steered = Arc::new(AtomicBool::new(false));
        let agent = runtime.agent().clone();
        let steered_for_listener = Arc::clone(&steered);
        runtime.agent().subscribe(Arc::new(
            move |event: AgentEvent, _signal: pi_core::AbortSignal| {
                let agent = agent.clone();
                let steered = Arc::clone(&steered_for_listener);
                async move {
                    if matches!(event, AgentEvent::TurnEnd { .. })
                        && !steered.swap(true, Ordering::SeqCst)
                    {
                        agent.steer(Message::User(UserMessage::text("steer", 1)));
                    }
                    Ok(())
                }
            },
        ));

        let outcome = runtime.prompt("start").await.unwrap();
        assert_eq!(
            assistant_texts(&outcome.new_messages),
            vec!["first", "second"]
        );
    }

    #[test]
    fn active_tools_switch_rebuilds_prompt_atomically() {
        let cwd = std::env::current_dir().unwrap();
        let runtime = PiRuntime::builder()
            .agent_plugin(pi_plugin_read::ReadPlugin)
            .agent_plugin(pi_plugin_write::WritePlugin)
            .agent_options(AgentOptions {
                active_tools: vec!["read".to_string()],
                cwd,
                ..AgentOptions::default()
            })
            .system_prompt(SystemPrompt::Pi(Box::default()))
            .build()
            .unwrap();
        runtime.set_active_tools(["write", "write"]).unwrap();
        let state = runtime.agent().state();
        assert_eq!(state.active_tools, vec!["write"]);
        assert!(
            state
                .system_prompt
                .contains("- write: Create or overwrite files")
        );
        assert!(!state.system_prompt.contains("- read: Read file contents"));

        let before = state;
        assert!(matches!(
            runtime.set_active_tools(["missing"]),
            Err(RuntimeError::UnknownTools(_))
        ));
        let after = runtime.agent().state();
        assert_eq!(after.active_tools, before.active_tools);
        assert_eq!(after.system_prompt, before.system_prompt);
    }

    #[test]
    fn resources_enable_pi_prompt_and_load_project_context() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("project");
        let agent_dir = root.path().join("agent");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(cwd.join("AGENTS.md"), "project rules").unwrap();
        let runtime = PiRuntime::builder()
            .agent_plugin(pi_plugin_read::ReadPlugin)
            .agent_options(AgentOptions {
                active_tools: vec!["read".to_string()],
                cwd: cwd.clone(),
                ..AgentOptions::default()
            })
            .resources(pi_resources::ResourceLoaderOptions::new(
                "ignored", &agent_dir,
            ))
            .build()
            .unwrap();
        let prompt = runtime.agent().state().system_prompt;
        assert!(prompt.contains("project rules"));
        assert!(prompt.contains(&format!("Current working directory: {}", cwd.display())));
        assert!(runtime.resource_diagnostics().is_empty());
    }

    #[test]
    fn resources_reject_final_system_prompt() {
        let error = match PiRuntime::builder()
            .system_prompt(SystemPrompt::Final("final".to_string()))
            .resources(pi_resources::ResourceLoaderOptions::new(".", "."))
            .build()
        {
            Ok(_) => panic!("resources with Final prompt must fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("Final prompt"));
    }

    #[test]
    fn final_system_prompt_overrides_agent_option_without_assembly() {
        let runtime = PiRuntime::builder()
            .agent_options(AgentOptions {
                system_prompt: "old".to_string(),
                ..AgentOptions::default()
            })
            .system_prompt(SystemPrompt::Final("final".to_string()))
            .build()
            .unwrap();
        assert_eq!(runtime.agent().state().system_prompt, "final");
    }

    #[test]
    fn pi_prompt_collects_only_active_tool_contributions() {
        let cwd = std::env::current_dir().unwrap();
        let runtime = PiRuntime::builder()
            .agent_plugin(pi_plugin_read::ReadPlugin)
            .agent_plugin(pi_plugin_write::WritePlugin)
            .agent_options(AgentOptions {
                active_tools: vec!["read".to_string()],
                cwd: cwd.clone(),
                ..AgentOptions::default()
            })
            .system_prompt(SystemPrompt::Pi(Box::new(
                pi_prompt::BuildSystemPromptOptions {
                    readme_path: Some("/pi/README.md".into()),
                    docs_path: Some("/pi/docs".into()),
                    examples_path: Some("/pi/examples".into()),
                    ..Default::default()
                },
            )))
            .build()
            .unwrap();
        let prompt = runtime.agent().state().system_prompt;
        assert!(prompt.contains("- read: Read file contents"));
        assert!(prompt.contains("Use read to examine files instead of cat or sed."));
        assert!(!prompt.contains("- write: Create or overwrite files"));
        assert!(prompt.contains(&format!("Current working directory: {}", cwd.display())));
    }

    fn assistant_texts(messages: &[Message]) -> Vec<&str> {
        messages
            .iter()
            .filter_map(|message| match message {
                Message::Assistant(message) => {
                    message.content.iter().find_map(|block| match block {
                        ContentBlock::Text(text) => Some(text.text.as_str()),
                        _ => None,
                    })
                }
                _ => None,
            })
            .collect()
    }
}
