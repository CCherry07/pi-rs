//! Session initialization options and model-selection policy.

use std::path::PathBuf;

use globset::GlobBuilder;
use pi_core::{ModelId, ModelSpec, PluginId, ProviderId, ScopedModel, ThinkingLevel};
use pi_runtime::PiRuntime;

use crate::{CompactionSettings, SessionContextBuildOptions, SessionModel, SessionPlugins};

#[derive(Clone, Default)]
pub struct AgentSessionOptions {
    pub context: SessionContextBuildOptions,
    pub plugins: SessionPlugins,
    pub compaction: CompactionSettings,
    /// Product-level model request to merge with a resumed session model.
    pub initial_model: InitialModelRequest,
    /// Generation-local tools the product enables when restoring a session.
    /// The product owns this policy; session replay only reconciles names
    /// against the new registry and persists the resulting selection.
    pub additional_active_tools: Vec<String>,
    /// Automatic compaction is enabled only when the model context window is
    /// known. Manual compaction remains available without this value.
    pub context_window: Option<u64>,
    /// Tokens reserved outside abandoned-branch summary input. The selected
    /// model context window supplies the total budget.
    pub branch_summary_reserve_tokens: Option<u64>,
    /// Immutable product registration metadata prepared alongside the runtime
    /// and session plugin generations.
    pub runtime_inventory: SessionRuntimeInventory,
    /// Pi v3-compatible parent session path recorded for a new session.
    pub parent_session_path: Option<PathBuf>,
    /// Exact adapter-provided ID for a new session.
    pub session_id: Option<String>,
    /// Generation-local defaults for shell shorthand execution. Explicit
    /// per-call shell paths still take precedence.
    pub shell_path: Option<PathBuf>,
    pub shell_command_prefix: Option<String>,
    /// Session-owned retry policy for transient assistant/provider failures.
    pub retry: AutoRetrySettings,
    pub(crate) initial_model_fallback_message: Option<String>,
}

/// Bounded, abortable retry policy used by normal assistant turns.
///
/// The initial provider call does not count toward `max_retries`; attempt one
/// waits `base_delay_ms`, attempt two waits twice that amount, and so on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoRetrySettings {
    pub enabled: bool,
    pub max_retries: u32,
    pub base_delay_ms: u64,
}

impl Default for AutoRetrySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_retries: 3,
            base_delay_ms: 2_000,
        }
    }
}

impl AgentSessionOptions {
    pub fn plugins(mut self, plugins: SessionPlugins) -> Self {
        self.plugins = plugins;
        self
    }

    pub fn compaction(mut self, compaction: CompactionSettings) -> Self {
        self.compaction = compaction;
        self
    }

    pub fn initial_model(mut self, request: InitialModelRequest) -> Self {
        self.initial_model = request;
        self
    }

    pub fn additional_active_tools(mut self, tools: Vec<String>) -> Self {
        self.additional_active_tools = tools;
        self
    }

    pub fn context_window(mut self, context_window: u64) -> Self {
        self.context_window = Some(context_window);
        self
    }

    pub fn branch_summary_reserve_tokens(mut self, reserve_tokens: u64) -> Self {
        self.branch_summary_reserve_tokens = Some(reserve_tokens);
        self
    }

    pub fn runtime_inventory(mut self, inventory: SessionRuntimeInventory) -> Self {
        self.runtime_inventory = inventory;
        self
    }

    pub fn shell(mut self, shell_path: Option<PathBuf>, command_prefix: Option<String>) -> Self {
        self.shell_path = shell_path;
        self.shell_command_prefix = command_prefix;
        self
    }

    pub fn retry(mut self, retry: AutoRetrySettings) -> Self {
        self.retry = retry;
        self
    }

    /// Carries the product's initial catalog fallback warning into the first
    /// frontend subscription without persisting it in Pi v4 storage.
    pub fn initial_model_fallback_message(mut self, message: Option<String>) -> Self {
        self.initial_model_fallback_message = message;
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionRuntimeInventory {
    js_extensions: Vec<String>,
    configured_native_plugins: Vec<PluginId>,
}

impl SessionRuntimeInventory {
    pub fn new(
        js_extensions: impl IntoIterator<Item = String>,
        configured_native_plugins: impl IntoIterator<Item = PluginId>,
    ) -> Self {
        Self {
            js_extensions: js_extensions.into_iter().collect(),
            configured_native_plugins: configured_native_plugins.into_iter().collect(),
        }
    }

    pub fn js_extensions(&self) -> &[String] {
        &self.js_extensions
    }

    pub fn configured_native_plugins(&self) -> &[PluginId] {
        &self.configured_native_plugins
    }
}

/// A model selected before an `AgentSession` restores the rest of its context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitialModelSelection {
    pub model: SessionModel,
    /// Present when a higher-priority session model could not be restored.
    pub fallback_message: Option<String>,
}

/// Inputs ordered by product policy rather than by registry implementation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InitialModelRequest {
    pub requested_provider: Option<ProviderId>,
    pub requested_model: Option<String>,
    pub session_model: Option<SessionModel>,
    pub settings_provider: Option<ProviderId>,
    pub settings_model: Option<String>,
}

impl InitialModelRequest {
    pub fn requested(mut self, provider: impl Into<ProviderId>, model: impl Into<String>) -> Self {
        self.requested_provider = Some(provider.into());
        self.requested_model = Some(model.into());
        self
    }

    pub fn session(mut self, model: Option<SessionModel>) -> Self {
        self.session_model = model;
        self
    }

    pub fn settings(mut self, provider: Option<ProviderId>, model: Option<String>) -> Self {
        self.settings_provider = provider;
        self.settings_model = model;
        self
    }

    /// Resolves this request against one immutable runtime generation without
    /// changing the runtime's active model.
    pub fn resolve(
        self,
        runtime: &PiRuntime,
    ) -> Result<InitialModelSelection, InitialModelResolveError> {
        let state = runtime.agent().state();
        InitialModelResolver::new(
            runtime.available_models(),
            SessionModel {
                provider: state.provider_id,
                model_id: state.model_id,
            },
        )
        .resolve(self)
    }

    /// Resolves this request and applies the selected model to the runtime.
    pub fn select(
        self,
        runtime: &PiRuntime,
    ) -> Result<InitialModelSelection, InitialModelResolveError> {
        let selection = self.resolve(runtime)?;
        let state = runtime.agent().state();
        if state.provider_id != selection.model.provider
            || state.model_id != selection.model.model_id
        {
            runtime
                .set_model(
                    selection.model.provider.clone(),
                    selection.model.model_id.clone(),
                )
                .map_err(|error| InitialModelResolveError::Apply(error.to_string()))?;
        }
        if let Some(model) = runtime.model(&selection.model.provider, &selection.model.model_id) {
            let effective = model.clamp_thinking_level(state.thinking_level);
            if effective != state.thinking_level {
                runtime
                    .set_thinking_level(effective)
                    .map_err(|error| InitialModelResolveError::Apply(error.to_string()))?;
            }
        }
        Ok(selection)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum InitialModelResolveError {
    #[error("requested model must not be empty")]
    EmptyRequestedModel,
    #[error("model {reference:?} is ambiguous; use provider/model ({matches})")]
    Ambiguous { reference: String, matches: String },
    #[error("model {reference:?} was not found in the registered model catalog")]
    NotFound { reference: String },
    #[error("failed to apply initial model: {0}")]
    Apply(String),
}

/// Pure initial-model policy over one immutable runtime catalog snapshot.
///
/// Priority is: explicit request, restorable session model, the current
/// settings default, first catalog model, then the runtime's configured
/// fallback. Loading models and resolving credentials remain responsibilities
/// of provider/catalog plugins.
struct InitialModelResolver {
    models: Vec<ModelSpec>,
    runtime_default: SessionModel,
}

impl InitialModelResolver {
    fn new(models: Vec<ModelSpec>, runtime_default: SessionModel) -> Self {
        Self {
            models,
            runtime_default,
        }
    }

    fn resolve(
        &self,
        request: InitialModelRequest,
    ) -> Result<InitialModelSelection, InitialModelResolveError> {
        if let Some(reference) = request.requested_model.as_deref() {
            return self.resolve_requested(request.requested_provider.as_ref(), reference);
        }

        let mut fallback_message = None;
        if let Some(session_model) = request.session_model {
            if self.models.is_empty() || self.contains(&session_model) {
                return Ok(InitialModelSelection {
                    model: session_model,
                    fallback_message: None,
                });
            }
            fallback_message = Some(format!(
                "Session model {}/{} is not in the registered catalog",
                session_model.provider, session_model.model_id
            ));
        }

        if let (Some(provider), Some(model_id)) =
            (request.settings_provider, request.settings_model)
        {
            if let Some(model) = self
                .models
                .iter()
                .find(|model| model.provider == provider && model.id.as_str() == model_id.trim())
            {
                return Ok(InitialModelSelection {
                    model: to_session_model(model),
                    fallback_message,
                });
            }
            let message = format!(
                "Settings model {}/{} is not in the registered model catalog",
                provider, model_id
            );
            fallback_message = Some(match fallback_message {
                Some(existing) => format!("{existing}; {message}"),
                None => message,
            });
        }

        if let Some(model) = self.models.first() {
            return Ok(InitialModelSelection {
                model: to_session_model(model),
                fallback_message: fallback_message
                    .map(|message| format!("{message}; using {}/{}", model.provider, model.id)),
            });
        }

        Ok(InitialModelSelection {
            model: self.runtime_default.clone(),
            fallback_message,
        })
    }

    fn resolve_requested(
        &self,
        requested_provider: Option<&ProviderId>,
        reference: &str,
    ) -> Result<InitialModelSelection, InitialModelResolveError> {
        let reference = reference.trim();
        if reference.is_empty() {
            return Err(InitialModelResolveError::EmptyRequestedModel);
        }

        if self.models.is_empty() {
            let provider = requested_provider
                .cloned()
                .unwrap_or_else(|| self.runtime_default.provider.clone());
            return Ok(requested_selection(provider, reference));
        }

        let mut matches = self.requested_matches(requested_provider, reference);
        matches.sort_by(|left, right| {
            left.provider
                .cmp(&right.provider)
                .then_with(|| left.id.cmp(&right.id))
        });
        matches.dedup_by(|left, right| left.provider == right.provider && left.id == right.id);

        match matches.as_slice() {
            [model] => Ok(InitialModelSelection {
                model: to_session_model(model),
                fallback_message: None,
            }),
            [] => {
                // An explicit provider may intentionally use a model that is
                // absent from the catalog (for example an OpenAI-compatible
                // endpoint). The runtime validates that the provider itself
                // is registered when the selection is applied.
                if let Some(provider) = requested_provider {
                    return Ok(requested_selection(provider.clone(), reference));
                }
                Err(InitialModelResolveError::NotFound {
                    reference: reference.to_string(),
                })
            }
            models => Err(InitialModelResolveError::Ambiguous {
                reference: reference.to_string(),
                matches: models
                    .iter()
                    .map(|model| format!("{}/{}", model.provider, model.id))
                    .collect::<Vec<_>>()
                    .join(", "),
            }),
        }
    }

    fn requested_matches<'a>(
        &'a self,
        requested_provider: Option<&ProviderId>,
        reference: &str,
    ) -> Vec<&'a ModelSpec> {
        if let Some(provider) = requested_provider {
            let model_reference = strip_provider_prefix(reference, provider).unwrap_or(reference);
            return self
                .models
                .iter()
                .filter(|model| {
                    eq_ignore_case(model.provider.as_str(), provider.as_str())
                        && model_matches(model, model_reference)
                })
                .collect();
        }

        if let Some((provider_reference, model_reference)) = reference.split_once('/')
            && self
                .models
                .iter()
                .any(|model| eq_ignore_case(model.provider.as_str(), provider_reference))
        {
            let provider_matches = self
                .models
                .iter()
                .filter(|model| {
                    eq_ignore_case(model.provider.as_str(), provider_reference)
                        && model_matches(model, model_reference)
                })
                .collect::<Vec<_>>();
            if !provider_matches.is_empty() {
                return provider_matches;
            }
        }

        // A model id may itself contain a slash. If a known provider prefix
        // did not produce a match, retry the complete reference as a raw id,
        // matching Pi's provider/model inference fallback.
        self.models
            .iter()
            .filter(|model| {
                model_matches(model, reference)
                    || eq_ignore_case(&format!("{}/{}", model.provider, model.id), reference)
            })
            .collect()
    }

    fn contains(&self, requested: &SessionModel) -> bool {
        self.models
            .iter()
            .any(|model| model.provider == requested.provider && model.id == requested.model_id)
    }
}

/// Resolves Pi `enabledModels`/`--models` patterns against an immutable
/// available catalog while preserving pattern and catalog order.
pub fn resolve_model_scope(patterns: &[String], models: &[ModelSpec]) -> Vec<ScopedModel> {
    let mut scoped = Vec::new();
    for raw_pattern in patterns {
        let pattern = raw_pattern.trim();
        if pattern.is_empty() {
            continue;
        }
        let (model_pattern, thinking_level) = split_thinking_level(pattern, models);
        let has_glob = model_pattern
            .chars()
            .any(|character| matches!(character, '*' | '?' | '['));
        if has_glob {
            let Ok(glob) = GlobBuilder::new(model_pattern)
                .case_insensitive(true)
                .literal_separator(false)
                .build()
            else {
                continue;
            };
            let matcher = glob.compile_matcher();
            for model in models {
                let full_id = format!("{}/{}", model.provider, model.id);
                if matcher.is_match(&full_id) || matcher.is_match(model.id.as_str()) {
                    push_scoped_model(&mut scoped, model, thinking_level);
                }
            }
        } else if let Some(model) = models.iter().find(|model| {
            eq_ignore_case(model.id.as_str(), model_pattern)
                || eq_ignore_case(&model.name, model_pattern)
                || eq_ignore_case(&format!("{}/{}", model.provider, model.id), model_pattern)
        }) {
            push_scoped_model(&mut scoped, model, thinking_level);
        }
    }
    scoped
}

fn split_thinking_level<'a>(
    pattern: &'a str,
    models: &[ModelSpec],
) -> (&'a str, Option<ThinkingLevel>) {
    let full_pattern_is_model = models.iter().any(|model| {
        eq_ignore_case(model.id.as_str(), pattern)
            || eq_ignore_case(&format!("{}/{}", model.provider, model.id), pattern)
    });
    if full_pattern_is_model {
        return (pattern, None);
    }
    let Some((model_pattern, suffix)) = pattern.rsplit_once(':') else {
        return (pattern, None);
    };
    let level = match suffix.to_ascii_lowercase().as_str() {
        "off" => ThinkingLevel::Off,
        "minimal" => ThinkingLevel::Minimal,
        "low" => ThinkingLevel::Low,
        "medium" => ThinkingLevel::Medium,
        "high" => ThinkingLevel::High,
        "xhigh" => ThinkingLevel::XHigh,
        "max" => ThinkingLevel::Max,
        _ => return (pattern, None),
    };
    (model_pattern, Some(level))
}

fn push_scoped_model(
    scoped: &mut Vec<ScopedModel>,
    model: &ModelSpec,
    thinking_level: Option<ThinkingLevel>,
) {
    if scoped.iter().any(|candidate| {
        candidate.model.provider == model.provider && candidate.model.id == model.id
    }) {
        return;
    }
    scoped.push(ScopedModel {
        model: model.clone(),
        thinking_level,
    });
}

fn requested_selection(provider: ProviderId, reference: &str) -> InitialModelSelection {
    let model_reference = strip_provider_prefix(reference, &provider).unwrap_or(reference);
    InitialModelSelection {
        model: SessionModel {
            provider,
            model_id: ModelId::new(model_reference),
        },
        fallback_message: None,
    }
}

fn to_session_model(model: &ModelSpec) -> SessionModel {
    SessionModel {
        provider: model.provider.clone(),
        model_id: model.id.clone(),
    }
}

fn strip_provider_prefix<'a>(reference: &'a str, provider: &ProviderId) -> Option<&'a str> {
    let (prefix, model) = reference.split_once('/')?;
    eq_ignore_case(prefix, provider.as_str()).then_some(model)
}

fn model_matches(model: &ModelSpec, reference: &str) -> bool {
    eq_ignore_case(model.id.as_str(), reference) || eq_ignore_case(&model.name, reference)
}

fn eq_ignore_case(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(provider: &str, id: &str, name: &str) -> ModelSpec {
        ModelSpec::new(provider, id, name, "openai-completions")
    }

    fn fallback() -> SessionModel {
        SessionModel {
            provider: ProviderId::new("fallback"),
            model_id: ModelId::new("fallback-model"),
        }
    }

    #[test]
    fn model_scope_preserves_pattern_order_thinking_levels_and_deduplicates() {
        let models = vec![
            model("anthropic", "claude-sonnet", "Sonnet"),
            model("anthropic", "claude-haiku", "Haiku"),
            model("gateway", "model:exact", "Colon Model"),
        ];

        let scoped = resolve_model_scope(
            &[
                "anthropic/*:high".to_string(),
                "model:exact".to_string(),
                "claude-sonnet".to_string(),
            ],
            &models,
        );

        assert_eq!(scoped.len(), 3);
        assert_eq!(scoped[0].model.id.as_str(), "claude-sonnet");
        assert_eq!(scoped[0].thinking_level, Some(ThinkingLevel::High));
        assert_eq!(scoped[1].model.id.as_str(), "claude-haiku");
        assert_eq!(scoped[1].thinking_level, Some(ThinkingLevel::High));
        assert_eq!(scoped[2].model.id.as_str(), "model:exact");
        assert_eq!(scoped[2].thinking_level, None);
    }

    #[test]
    fn initial_model_selection_clamps_thinking_to_model_capabilities() {
        struct Catalog;

        #[pi_core::provider_plugin]
        impl pi_core::ProviderPlugin for Catalog {
            fn id(&self) -> pi_core::PluginId {
                pi_core::PluginId::new("thinking-catalog")
            }

            fn register(
                &self,
                context: &mut pi_core::ProviderRegisterContext<'_>,
            ) -> pi_core::Result<()> {
                let mut model = ModelSpec::new("scripted", "sparse", "Sparse", "test");
                model.reasoning = true;
                for level in ["minimal", "low", "medium"] {
                    model.thinking_level_map.insert(level.to_string(), None);
                }
                context.register_model(model)
            }
        }

        let runtime = PiRuntime::builder()
            .provider_plugin(pi_test_support::ScriptedProviderPlugin::scripted([]))
            .provider_plugin(Catalog)
            .agent_options(pi_agent::AgentOptions {
                provider_id: ProviderId::new("scripted"),
                model_id: ModelId::new("test"),
                thinking_level: ThinkingLevel::Low,
                ..pi_agent::AgentOptions::default()
            })
            .build()
            .unwrap();

        InitialModelRequest::default()
            .requested("scripted", "sparse")
            .select(&runtime)
            .unwrap();

        assert_eq!(runtime.agent().state().thinking_level, ThinkingLevel::High);
    }

    #[test]
    fn explicit_unique_model_id_wins_across_providers() {
        let resolver = InitialModelResolver::new(
            vec![model("one", "alpha", "Alpha"), model("two", "beta", "Beta")],
            fallback(),
        );

        let selected = resolver
            .resolve(InitialModelRequest {
                requested_model: Some("beta".to_string()),
                ..InitialModelRequest::default()
            })
            .unwrap();

        assert_eq!(selected.model.provider.as_str(), "two");
        assert_eq!(selected.model.model_id.as_str(), "beta");
    }

    #[test]
    fn explicit_provider_allows_a_custom_model_id() {
        let resolver =
            InitialModelResolver::new(vec![model("other", "registered", "Registered")], fallback());

        let selected = resolver
            .resolve(InitialModelRequest::default().requested("custom", "unlisted"))
            .unwrap();

        assert_eq!(selected.model.provider.as_str(), "custom");
        assert_eq!(selected.model.model_id.as_str(), "unlisted");
    }

    #[test]
    fn ambiguous_bare_model_requires_a_provider() {
        let resolver = InitialModelResolver::new(
            vec![
                model("one", "shared", "Shared"),
                model("two", "shared", "Shared"),
            ],
            fallback(),
        );

        let error = resolver
            .resolve(InitialModelRequest {
                requested_model: Some("shared".to_string()),
                ..InitialModelRequest::default()
            })
            .unwrap_err();

        assert!(matches!(error, InitialModelResolveError::Ambiguous { .. }));
        assert!(error.to_string().contains("one/shared, two/shared"));
    }

    #[test]
    fn slash_model_id_falls_back_after_provider_inference_misses() {
        let resolver = InitialModelResolver::new(
            vec![
                model("openai", "different", "Different"),
                model("gateway", "openai/gpt-4o", "GPT-4o via Gateway"),
            ],
            fallback(),
        );

        let selected = resolver
            .resolve(InitialModelRequest {
                requested_model: Some("openai/gpt-4o".to_string()),
                ..InitialModelRequest::default()
            })
            .unwrap();

        assert_eq!(selected.model.provider.as_str(), "gateway");
        assert_eq!(selected.model.model_id.as_str(), "openai/gpt-4o");
    }

    #[test]
    fn session_model_wins_when_it_still_exists() {
        let resolver = InitialModelResolver::new(
            vec![model("one", "alpha", "Alpha"), model("two", "beta", "Beta")],
            fallback(),
        );

        let selected = resolver
            .resolve(InitialModelRequest::default().session(Some(SessionModel {
                provider: ProviderId::new("two"),
                model_id: ModelId::new("beta"),
            })))
            .unwrap();

        assert_eq!(selected.model.provider.as_str(), "two");
    }

    #[test]
    fn restorable_session_model_precedes_the_settings_default() {
        let resolver = InitialModelResolver::new(
            vec![model("one", "alpha", "Alpha"), model("two", "beta", "Beta")],
            fallback(),
        );

        let selected = resolver
            .resolve(
                InitialModelRequest::default()
                    .session(Some(SessionModel {
                        provider: ProviderId::new("two"),
                        model_id: ModelId::new("beta"),
                    }))
                    .settings(Some(ProviderId::new("one")), Some("alpha".to_string())),
            )
            .unwrap();

        assert_eq!(selected.model.provider.as_str(), "two");
        assert_eq!(selected.model.model_id.as_str(), "beta");
    }

    #[test]
    fn settings_default_precedes_the_catalog_default() {
        let resolver = InitialModelResolver::new(
            vec![model("one", "alpha", "Alpha"), model("two", "beta", "Beta")],
            fallback(),
        );

        let selected = resolver
            .resolve(
                InitialModelRequest::default()
                    .settings(Some(ProviderId::new("two")), Some("beta".to_string())),
            )
            .unwrap();

        assert_eq!(selected.model.provider.as_str(), "two");
        assert_eq!(selected.model.model_id.as_str(), "beta");
    }

    #[test]
    fn missing_settings_default_falls_back_with_a_diagnostic() {
        let resolver =
            InitialModelResolver::new(vec![model("catalog", "first", "First")], fallback());

        let selected = resolver
            .resolve(
                InitialModelRequest::default()
                    .settings(Some(ProviderId::new("removed")), Some("old".to_string())),
            )
            .unwrap();

        assert_eq!(selected.model.provider.as_str(), "catalog");
        let diagnostic = selected.fallback_message.expect("fallback diagnostic");
        assert!(diagnostic.contains("Settings model removed/old"));
        assert!(diagnostic.contains("using catalog/first"));
    }

    #[test]
    fn removed_session_model_falls_back_with_a_diagnostic() {
        let resolver =
            InitialModelResolver::new(vec![model("catalog", "first", "First")], fallback());

        let selected = resolver
            .resolve(InitialModelRequest::default().session(Some(SessionModel {
                provider: ProviderId::new("removed"),
                model_id: ModelId::new("old"),
            })))
            .unwrap();

        assert_eq!(selected.model.provider.as_str(), "catalog");
        assert!(selected.fallback_message.is_some());
    }

    #[test]
    fn catalog_default_precedes_the_runtime_fallback() {
        let resolver =
            InitialModelResolver::new(vec![model("catalog", "first", "First")], fallback());
        let selected = resolver.resolve(InitialModelRequest::default()).unwrap();

        assert_eq!(selected.model.provider.as_str(), "catalog");
    }

    #[test]
    fn empty_catalog_preserves_the_runtime_fallback() {
        let resolver = InitialModelResolver::new(Vec::new(), fallback());
        let selected = resolver.resolve(InitialModelRequest::default()).unwrap();

        assert_eq!(selected.model, fallback());
    }
}
