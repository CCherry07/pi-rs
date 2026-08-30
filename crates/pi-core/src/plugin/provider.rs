use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use super::agent::{PluginDiagnostic, PluginDiagnosticSink, PluginError};
use super::capabilities::{
    ContextParts, ModelsContext, PluginContextEpoch, PluginContextHandle, SessionContext, UiContext,
};
use crate::{
    AbortSignal, CoreError, ModelId, ModelSpec, PluginId, Provider, ProviderId, RegistriesBuilder,
    Result,
};

#[derive(Clone)]
pub struct ProviderPluginContext {
    plugin_id: PluginId,
    generation: u64,
    provider_id: ProviderId,
    model_id: ModelId,
    cwd: PathBuf,
    abort_signal: AbortSignal,
    pub session: SessionContext,
    pub models: ModelsContext,
    pub ui: UiContext,
    diagnostics: PluginDiagnosticSink,
}

impl ProviderPluginContext {
    #[allow(clippy::too_many_arguments)]
    #[doc(hidden)]
    pub fn unavailable_for_testing(
        plugin_id: PluginId,
        generation: u64,
        provider_id: ProviderId,
        model_id: ModelId,
        cwd: PathBuf,
        abort_signal: AbortSignal,
    ) -> Self {
        let context = ContextParts::unavailable();
        Self {
            plugin_id,
            generation,
            provider_id,
            model_id,
            cwd,
            abort_signal,
            session: context.session,
            models: context.models,
            ui: context.ui,
            diagnostics: PluginDiagnosticSink::default(),
        }
    }

    pub fn plugin_id(&self) -> &PluginId {
        &self.plugin_id
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    pub fn model_id(&self) -> &ModelId {
        &self.model_id
    }

    pub fn cwd(&self) -> &std::path::Path {
        &self.cwd
    }

    pub fn signal(&self) -> &AbortSignal {
        &self.abort_signal
    }

    pub fn report_hook_error(&self, hook: &'static str, message: impl Into<String>) {
        self.diagnostics
            .record(self.plugin_id.clone(), hook, message);
    }

    #[doc(hidden)]
    pub fn plugin_context_handle(&self) -> PluginContextHandle {
        self.session.handle_for_adapter()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BeforeProviderRequestEvent {
    pub payload: Value,
}

/// Final provider HTTP headers immediately before transport dispatch.
///
/// `None` preserves Pi's JavaScript extension convention that assigning
/// `null` deletes a header. The driver keeps tombstones visible while hooks
/// chain and removes them only when returning the transport-ready map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeforeProviderHeadersEvent {
    pub headers: BTreeMap<String, Option<String>>,
}

/// Provider response metadata observed before its body stream is consumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AfterProviderResponseEvent {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
}

/// Registration surface reserved for provider plugins.
///
/// Provider plugins deliberately do not participate in agent input or
/// lifecycle hooks. A single plugin may register multiple provider variants.
pub struct ProviderRegisterContext<'a> {
    owner: PluginId,
    registries: &'a mut RegistriesBuilder,
}

impl<'a> ProviderRegisterContext<'a> {
    fn new(owner: PluginId, registries: &'a mut RegistriesBuilder) -> Self {
        Self { owner, registries }
    }

    pub fn register_provider(&mut self, provider: Arc<dyn Provider>) -> Result<()> {
        self.registries
            .register_provider(self.owner.clone(), provider)
    }

    /// Returns a provider registered by an earlier provider plugin. A later
    /// plugin can use it as the fallback for a configuration overlay.
    pub fn base_provider(&self, id: &ProviderId) -> Option<Arc<dyn Provider>> {
        self.registries.provider(id)
    }

    /// Adds or replaces the provider selected for this generation. Two
    /// provider plugins may not override the same provider.
    pub fn register_provider_override(&mut self, provider: Arc<dyn Provider>) -> Result<()> {
        self.registries
            .register_provider_override(self.owner.clone(), provider)
    }

    pub fn register_model(&mut self, model: ModelSpec) -> Result<()> {
        self.registries.register_model(self.owner.clone(), model)
    }

    /// Returns model metadata registered by earlier provider plugins for one
    /// provider. A later catalog overlay can use this immutable snapshot to
    /// compose explicit user configuration.
    pub fn base_models(&self, provider: &ProviderId) -> Vec<ModelSpec> {
        self.registries.models_for_provider(provider)
    }

    /// Replaces one model registered by an earlier provider plugin. A model
    /// may be overridden at most once while constructing a generation.
    pub fn register_model_override(&mut self, model: ModelSpec) -> Result<()> {
        self.registries
            .register_model_override(self.owner.clone(), model)
    }

    /// Replaces the complete model catalog for one provider while this
    /// generation is being constructed. The published registry remains
    /// immutable, and at most one plugin may replace a provider catalog.
    pub fn replace_provider_models(
        &mut self,
        provider: ProviderId,
        models: Vec<ModelSpec>,
    ) -> Result<()> {
        self.registries
            .replace_provider_models(self.owner.clone(), provider, models)
    }
}

/// A provider-system plugin that contributes providers, routing overlays,
/// model catalog entries, and provider request lifecycle hooks.
///
/// Statically linked implementations use `#[pi_core::provider_plugin]`, which
/// supplies the async-trait expansion.
#[async_trait]
pub trait ProviderPlugin: Send + Sync {
    fn id(&self) -> PluginId;

    fn register(&self, _context: &mut ProviderRegisterContext<'_>) -> Result<()> {
        Ok(())
    }

    /// Runs after a concrete provider has serialized its final wire payload and
    /// immediately before that payload is sent. Returning `None` preserves the
    /// current payload; returning `Some` replaces it for later plugins and the
    /// request itself.
    async fn before_provider_request(
        &self,
        _context: ProviderPluginContext,
        _event: BeforeProviderRequestEvent,
    ) -> std::result::Result<Option<Value>, PluginError> {
        Ok(None)
    }

    /// Runs after a concrete provider has assembled its final HTTP headers and
    /// immediately before transport. Returning `None` preserves the current
    /// map; returning `Some` replaces it for later plugins and transport.
    async fn before_provider_headers(
        &self,
        _context: ProviderPluginContext,
        _event: BeforeProviderHeadersEvent,
    ) -> std::result::Result<Option<BTreeMap<String, Option<String>>>, PluginError> {
        Ok(None)
    }

    /// Runs after an HTTP response arrives and before its body stream is
    /// consumed. Observer failures are diagnostic and never replace the
    /// provider result.
    async fn after_provider_response(
        &self,
        _context: ProviderPluginContext,
        _event: AfterProviderResponseEvent,
    ) -> std::result::Result<(), PluginError> {
        Ok(())
    }
}

struct RegisteredProviderPlugin {
    id: PluginId,
    plugin: Arc<dyn ProviderPlugin>,
}

/// Immutable, generation-local provider plugin set.
pub struct ProviderPluginDriver {
    plugins: Vec<RegisteredProviderPlugin>,
    diagnostics: PluginDiagnosticSink,
    context_epoch: PluginContextEpoch,
}

impl ProviderPluginDriver {
    pub fn new(plugins: Vec<Arc<dyn ProviderPlugin>>) -> Result<Self> {
        Self::new_with_context(plugins, PluginContextEpoch::unavailable())
    }

    pub fn new_with_context(
        plugins: Vec<Arc<dyn ProviderPlugin>>,
        context_epoch: PluginContextEpoch,
    ) -> Result<Self> {
        let mut seen = std::collections::HashSet::new();
        let mut registered = Vec::with_capacity(plugins.len());
        for plugin in plugins {
            let id = plugin.id();
            if !seen.insert(id.clone()) {
                return Err(CoreError::DuplicateProviderPlugin(id.to_string()));
            }
            registered.push(RegisteredProviderPlugin { id, plugin });
        }
        Ok(Self {
            plugins: registered,
            diagnostics: PluginDiagnosticSink::default(),
            context_epoch,
        })
    }

    pub fn plugin_order(&self) -> Vec<PluginId> {
        self.plugins
            .iter()
            .map(|plugin| plugin.id.clone())
            .collect()
    }

    pub fn diagnostics(&self) -> Vec<PluginDiagnostic> {
        self.diagnostics.snapshot()
    }

    pub fn take_diagnostics(&self) -> Vec<PluginDiagnostic> {
        self.diagnostics.take()
    }

    pub fn register_all(&self, registries: &mut RegistriesBuilder) -> Result<()> {
        for registered in &self.plugins {
            let mut context = ProviderRegisterContext::new(registered.id.clone(), registries);
            registered.plugin.register(&mut context)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn before_provider_request(
        &self,
        generation: u64,
        provider_id: &ProviderId,
        model_id: &ModelId,
        cwd: &std::path::Path,
        signal: &AbortSignal,
        mut payload: Value,
    ) -> std::result::Result<Value, PluginError> {
        for registered in &self.plugins {
            let context = self.context_epoch.context();
            let event = BeforeProviderRequestEvent {
                payload: payload.clone(),
            };
            let replacement = registered
                .plugin
                .before_provider_request(
                    ProviderPluginContext {
                        plugin_id: registered.id.clone(),
                        generation,
                        provider_id: provider_id.clone(),
                        model_id: model_id.clone(),
                        cwd: cwd.to_path_buf(),
                        abort_signal: signal.clone(),
                        session: context.session,
                        models: context.models,
                        ui: context.ui,
                        diagnostics: self.diagnostics.clone(),
                    },
                    event,
                )
                .await;
            let replacement = match replacement {
                Ok(replacement) => replacement,
                Err(error) => {
                    self.diagnostics.record(
                        registered.id.clone(),
                        "before_provider_request",
                        error.to_string(),
                    );
                    continue;
                }
            };
            if let Some(replacement) = replacement {
                payload = replacement;
            }
        }
        Ok(payload)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn before_provider_headers(
        &self,
        generation: u64,
        provider_id: &ProviderId,
        model_id: &ModelId,
        cwd: &std::path::Path,
        signal: &AbortSignal,
        headers: BTreeMap<String, String>,
    ) -> BTreeMap<String, String> {
        let mut headers = headers
            .into_iter()
            .map(|(name, value)| (name, Some(value)))
            .collect::<BTreeMap<_, _>>();
        for registered in &self.plugins {
            let context = self.context_epoch.context();
            let event = BeforeProviderHeadersEvent {
                headers: headers.clone(),
            };
            let replacement = registered
                .plugin
                .before_provider_headers(
                    ProviderPluginContext {
                        plugin_id: registered.id.clone(),
                        generation,
                        provider_id: provider_id.clone(),
                        model_id: model_id.clone(),
                        cwd: cwd.to_path_buf(),
                        abort_signal: signal.clone(),
                        session: context.session,
                        models: context.models,
                        ui: context.ui,
                        diagnostics: self.diagnostics.clone(),
                    },
                    event,
                )
                .await;
            match replacement {
                Ok(Some(replacement)) => headers = replacement,
                Ok(None) => {}
                Err(error) => self.diagnostics.record(
                    registered.id.clone(),
                    "before_provider_headers",
                    error.to_string(),
                ),
            }
        }
        headers
            .into_iter()
            .filter_map(|(name, value)| value.map(|value| (name, value)))
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn after_provider_response(
        &self,
        generation: u64,
        provider_id: &ProviderId,
        model_id: &ModelId,
        cwd: &std::path::Path,
        signal: &AbortSignal,
        status: u16,
        headers: BTreeMap<String, String>,
    ) {
        for registered in &self.plugins {
            let context = self.context_epoch.context();
            let result = registered
                .plugin
                .after_provider_response(
                    ProviderPluginContext {
                        plugin_id: registered.id.clone(),
                        generation,
                        provider_id: provider_id.clone(),
                        model_id: model_id.clone(),
                        cwd: cwd.to_path_buf(),
                        abort_signal: signal.clone(),
                        session: context.session,
                        models: context.models,
                        ui: context.ui,
                        diagnostics: self.diagnostics.clone(),
                    },
                    AfterProviderResponseEvent {
                        status,
                        headers: headers.clone(),
                    },
                )
                .await;
            if let Err(error) = result {
                self.diagnostics.record(
                    registered.id.clone(),
                    "after_provider_response",
                    error.to_string(),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AbortSignal, ModelId, ProviderError, ProviderRequest, ProviderStream};
    use std::sync::Mutex;

    struct TestProvider(&'static str);

    #[async_trait::async_trait]
    impl Provider for TestProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new(self.0)
        }

        async fn stream(
            &self,
            _request: ProviderRequest,
            _context: crate::ProviderCallContext,
            _signal: AbortSignal,
        ) -> std::result::Result<ProviderStream, ProviderError> {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    struct CatalogProviderPlugin;

    struct BaseCatalogProviderPlugin;

    #[pi_core::provider_plugin]
    impl ProviderPlugin for BaseCatalogProviderPlugin {
        fn id(&self) -> PluginId {
            PluginId::new("base-catalog")
        }

        fn register(&self, context: &mut ProviderRegisterContext<'_>) -> Result<()> {
            context.register_provider(Arc::new(TestProvider("replaceable")))?;
            context.register_model(ModelSpec::new(
                "replaceable",
                "first",
                "First",
                "openai-completions",
            ))?;
            context.register_model(ModelSpec::new(
                "replaceable",
                "second",
                "Second",
                "openai-completions",
            ))
        }
    }

    struct ReplacementCatalogProviderPlugin;

    #[pi_core::provider_plugin]
    impl ProviderPlugin for ReplacementCatalogProviderPlugin {
        fn id(&self) -> PluginId {
            PluginId::new("replacement-catalog")
        }

        fn register(&self, context: &mut ProviderRegisterContext<'_>) -> Result<()> {
            let provider = ProviderId::new("replaceable");
            assert_eq!(context.base_models(&provider).len(), 2);
            context.register_provider_override(Arc::new(TestProvider("replaceable")))?;
            context.replace_provider_models(
                provider,
                vec![ModelSpec::new(
                    "replaceable",
                    "replacement",
                    "Replacement",
                    "openai-completions",
                )],
            )
        }
    }

    struct BaseProviderPlugin;

    struct PayloadPlugin {
        id: &'static str,
        field: &'static str,
    }

    struct FailingPayloadPlugin;

    struct WirePlugin {
        id: &'static str,
        observations: Arc<Mutex<Vec<String>>>,
    }

    struct FailingWirePlugin;

    #[pi_core::provider_plugin]
    impl ProviderPlugin for PayloadPlugin {
        fn id(&self) -> PluginId {
            PluginId::new(self.id)
        }

        async fn before_provider_request(
            &self,
            _context: ProviderPluginContext,
            event: BeforeProviderRequestEvent,
        ) -> std::result::Result<Option<Value>, PluginError> {
            let mut payload = event.payload;
            payload[self.field] = Value::String(self.id.to_string());
            Ok(Some(payload))
        }
    }

    #[pi_core::provider_plugin]
    impl ProviderPlugin for FailingPayloadPlugin {
        fn id(&self) -> PluginId {
            PluginId::new("failing")
        }

        async fn before_provider_request(
            &self,
            _context: ProviderPluginContext,
            _event: BeforeProviderRequestEvent,
        ) -> std::result::Result<Option<Value>, PluginError> {
            Err(PluginError::Registration("intentional failure".to_string()))
        }
    }

    #[pi_core::provider_plugin]
    impl ProviderPlugin for WirePlugin {
        fn id(&self) -> PluginId {
            PluginId::new(self.id)
        }

        async fn before_provider_headers(
            &self,
            _context: ProviderPluginContext,
            event: BeforeProviderHeadersEvent,
        ) -> std::result::Result<Option<BTreeMap<String, Option<String>>>, PluginError> {
            if self.id == "second" {
                assert_eq!(event.headers.get("X-Remove"), Some(&None));
            }
            let mut headers = event.headers;
            headers.insert(format!("X-{}", self.id), Some(self.id.to_string()));
            if self.id == "first" {
                headers.insert("X-Remove".to_string(), None);
            }
            Ok(Some(headers))
        }

        async fn after_provider_response(
            &self,
            _context: ProviderPluginContext,
            event: AfterProviderResponseEvent,
        ) -> std::result::Result<(), PluginError> {
            self.observations.lock().unwrap().push(format!(
                "{}:{}:{}",
                self.id, event.status, event.headers["x-request-id"]
            ));
            Ok(())
        }
    }

    #[pi_core::provider_plugin]
    impl ProviderPlugin for FailingWirePlugin {
        fn id(&self) -> PluginId {
            PluginId::new("failing-wire")
        }

        async fn before_provider_headers(
            &self,
            _context: ProviderPluginContext,
            _event: BeforeProviderHeadersEvent,
        ) -> std::result::Result<Option<BTreeMap<String, Option<String>>>, PluginError> {
            Err(PluginError::Registration("header failure".to_string()))
        }

        async fn after_provider_response(
            &self,
            _context: ProviderPluginContext,
            _event: AfterProviderResponseEvent,
        ) -> std::result::Result<(), PluginError> {
            Err(PluginError::Registration("response failure".to_string()))
        }
    }

    #[pi_core::provider_plugin]
    impl ProviderPlugin for BaseProviderPlugin {
        fn id(&self) -> PluginId {
            PluginId::new("base")
        }

        fn register(&self, context: &mut ProviderRegisterContext<'_>) -> Result<()> {
            context.register_provider(Arc::new(TestProvider("custom")))
        }
    }

    #[pi_core::provider_plugin]
    impl ProviderPlugin for CatalogProviderPlugin {
        fn id(&self) -> PluginId {
            PluginId::new("models")
        }

        fn register(&self, context: &mut ProviderRegisterContext<'_>) -> Result<()> {
            assert!(context.base_provider(&ProviderId::new("custom")).is_some());
            context.register_provider_override(Arc::new(TestProvider("custom")))?;
            context.register_model(ModelSpec::new(
                "custom",
                "model",
                "Model",
                "openai-completions",
            ))
        }
    }

    #[test]
    fn provider_plugin_contribution_freezes_provider_and_catalog_together() {
        let (_, _, registries) = RegistriesBuilder::new()
            .register_plugin_sets(
                Vec::new(),
                vec![
                    Arc::new(BaseProviderPlugin),
                    Arc::new(CatalogProviderPlugin),
                ],
            )
            .unwrap();
        let provider = ProviderId::new("custom");
        let model = ModelId::new("model");

        assert!(registries.provider(&provider).is_some());
        assert_eq!(
            registries.provider_owner(&provider),
            Some(&PluginId::new("models"))
        );
        assert_eq!(registries.model(&provider, &model).unwrap().name, "Model");
        assert_eq!(
            registries.model_owner(&provider, &model),
            Some(&PluginId::new("models"))
        );
    }

    #[test]
    fn provider_catalog_replacement_removes_lower_layer_models_before_freeze() {
        let (_, _, registries) = RegistriesBuilder::new()
            .register_plugin_sets(
                Vec::new(),
                vec![
                    Arc::new(BaseCatalogProviderPlugin),
                    Arc::new(ReplacementCatalogProviderPlugin),
                ],
            )
            .unwrap();
        let provider = ProviderId::new("replaceable");

        assert!(
            registries
                .model(&provider, &ModelId::new("first"))
                .is_none()
        );
        assert!(
            registries
                .model(&provider, &ModelId::new("second"))
                .is_none()
        );
        assert_eq!(
            registries
                .model(&provider, &ModelId::new("replacement"))
                .unwrap()
                .name,
            "Replacement"
        );
    }

    #[tokio::test]
    async fn request_hooks_chain_in_provider_plugin_order_without_registering_a_provider() {
        let driver = ProviderPluginDriver::new(vec![
            Arc::new(PayloadPlugin {
                id: "first",
                field: "first",
            }),
            Arc::new(PayloadPlugin {
                id: "second",
                field: "second",
            }),
        ])
        .unwrap();
        let (_, signal) = crate::AbortHandle::new();

        let payload = driver
            .before_provider_request(
                7,
                &ProviderId::new("openai-compatible"),
                &ModelId::new("model"),
                std::path::Path::new("/project"),
                &signal,
                serde_json::json!({ "existing": true }),
            )
            .await
            .unwrap();

        assert_eq!(
            payload,
            serde_json::json!({
                "existing": true,
                "first": "first",
                "second": "second"
            })
        );
    }

    #[tokio::test]
    async fn request_hook_failures_are_diagnostic_and_later_hooks_still_run() {
        let driver = ProviderPluginDriver::new(vec![
            Arc::new(PayloadPlugin {
                id: "first",
                field: "first",
            }),
            Arc::new(FailingPayloadPlugin),
            Arc::new(PayloadPlugin {
                id: "second",
                field: "second",
            }),
        ])
        .unwrap();
        let (_, signal) = crate::AbortHandle::new();

        let payload = driver
            .before_provider_request(
                1,
                &ProviderId::new("provider"),
                &ModelId::new("model"),
                std::path::Path::new("/workspace"),
                &signal,
                serde_json::json!({}),
            )
            .await
            .unwrap();

        assert_eq!(
            payload,
            serde_json::json!({"first": "first", "second": "second"})
        );
        assert!(driver.diagnostics().iter().any(|diagnostic| {
            diagnostic.plugin_id == PluginId::new("failing")
                && diagnostic.hook == "before_provider_request"
                && diagnostic.message.contains("intentional failure")
        }));
    }

    #[tokio::test]
    async fn wire_hooks_chain_header_tombstones_and_isolate_observer_failures() {
        let observations = Arc::new(Mutex::new(Vec::new()));
        let driver = ProviderPluginDriver::new(vec![
            Arc::new(WirePlugin {
                id: "first",
                observations: Arc::clone(&observations),
            }),
            Arc::new(FailingWirePlugin),
            Arc::new(WirePlugin {
                id: "second",
                observations: Arc::clone(&observations),
            }),
        ])
        .unwrap();
        let (_, signal) = crate::AbortHandle::new();
        let provider = ProviderId::new("provider");
        let model = ModelId::new("model");

        let headers = driver
            .before_provider_headers(
                8,
                &provider,
                &model,
                std::path::Path::new("/workspace"),
                &signal,
                BTreeMap::from([
                    ("Existing".to_string(), "yes".to_string()),
                    ("X-Remove".to_string(), "remove-me".to_string()),
                ]),
            )
            .await;
        assert_eq!(headers["Existing"], "yes");
        assert_eq!(headers["X-first"], "first");
        assert_eq!(headers["X-second"], "second");
        assert!(!headers.contains_key("X-Remove"));

        driver
            .after_provider_response(
                8,
                &provider,
                &model,
                std::path::Path::new("/workspace"),
                &signal,
                429,
                BTreeMap::from([("x-request-id".to_string(), "request-1".to_string())]),
            )
            .await;
        assert_eq!(
            *observations.lock().unwrap(),
            vec!["first:429:request-1", "second:429:request-1"]
        );
        assert!(driver.diagnostics().iter().any(|diagnostic| {
            diagnostic.plugin_id == PluginId::new("failing-wire")
                && diagnostic.hook == "before_provider_headers"
                && diagnostic.message.contains("header failure")
        }));
        assert!(driver.diagnostics().iter().any(|diagnostic| {
            diagnostic.plugin_id == PluginId::new("failing-wire")
                && diagnostic.hook == "after_provider_response"
                && diagnostic.message.contains("response failure")
        }));
    }
}
