use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use pi_core::{AgentPlugin, PluginId, ProviderPlugin};
use pi_runtime::PiRuntimeBuilder;

use crate::{SessionPlugin, SessionPlugins};

/// Version of the in-process plugin host contract understood by this build.
pub const PLUGIN_HOST_API_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PluginCapability {
    Agent,
    Provider,
    Session,
}

impl PluginCapability {
    fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Provider => "provider",
            Self::Session => "session",
        }
    }
}

/// Package metadata shared by every contribution in one plugin bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginManifest {
    id: PluginId,
    version: String,
    host_api_version: u32,
    capabilities: BTreeSet<PluginCapability>,
}

impl PluginManifest {
    pub fn new(id: impl Into<PluginId>, version: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            version: version.into(),
            host_api_version: PLUGIN_HOST_API_VERSION,
            capabilities: BTreeSet::new(),
        }
    }

    pub fn host_api_version(mut self, version: u32) -> Self {
        self.host_api_version = version;
        self
    }

    pub fn id(&self) -> &PluginId {
        &self.id
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn capabilities(&self) -> &BTreeSet<PluginCapability> {
        &self.capabilities
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PluginBundleError {
    #[error("plugin bundle id cannot be empty")]
    EmptyId,
    #[error("plugin bundle {plugin_id} version cannot be empty")]
    EmptyVersion { plugin_id: PluginId },
    #[error(
        "plugin bundle {plugin_id} requires host API {requested}, but this host provides {supported}"
    )]
    IncompatibleHostApi {
        plugin_id: PluginId,
        requested: u32,
        supported: u32,
    },
    #[error("plugin bundle {plugin_id} has no contributions")]
    EmptyBundle { plugin_id: PluginId },
    #[error("plugin bundle {plugin_id} already has a {capability} contribution")]
    DuplicateContribution {
        plugin_id: PluginId,
        capability: &'static str,
    },
    #[error("duplicate plugin bundle id: {0}")]
    DuplicateBundle(PluginId),
}

#[derive(Debug, thiserror::Error)]
pub enum PluginBundleLoadError {
    #[error("plugin bundle {plugin_id} failed to load its {capability} contribution: {message}")]
    Factory {
        plugin_id: PluginId,
        capability: &'static str,
        message: String,
    },
    #[error(
        "plugin bundle {plugin_id} loaded a {capability} contribution with mismatched id {actual_id}"
    )]
    ContributionIdMismatch {
        plugin_id: PluginId,
        capability: &'static str,
        actual_id: PluginId,
    },
}

type AgentPluginFactory =
    Arc<dyn Fn() -> Result<Arc<dyn AgentPlugin>, PluginBundleLoadError> + Send + Sync>;
type ProviderPluginFactory =
    Arc<dyn Fn() -> Result<Arc<dyn ProviderPlugin>, PluginBundleLoadError> + Send + Sync>;
type SessionPluginFactory =
    Arc<dyn Fn() -> Result<Arc<dyn SessionPlugin>, PluginBundleLoadError> + Send + Sync>;

/// One installable package with up to one contribution to each plugin system.
///
/// Contributions are factories rather than instances so installing a bundle
/// preserves the existing generation-based reload semantics.
#[derive(Clone)]
pub struct PluginBundle {
    manifest: PluginManifest,
    agent: Option<AgentPluginFactory>,
    provider: Option<ProviderPluginFactory>,
    session: Option<SessionPluginFactory>,
}

impl std::fmt::Debug for PluginBundle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PluginBundle")
            .field("manifest", &self.manifest)
            .field("has_agent", &self.agent.is_some())
            .field("has_provider", &self.provider.is_some())
            .field("has_session", &self.session.is_some())
            .finish()
    }
}

impl PluginBundle {
    pub fn new(manifest: PluginManifest) -> Self {
        Self {
            manifest,
            agent: None,
            provider: None,
            session: None,
        }
    }

    pub fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    pub fn agent_plugin<F, P>(self, factory: F) -> Result<Self, PluginBundleError>
    where
        F: Fn() -> P + Send + Sync + 'static,
        P: AgentPlugin + 'static,
    {
        self.try_agent_plugin(move || Ok::<P, std::convert::Infallible>(factory()))
    }

    pub fn try_agent_plugin<F, P, E>(mut self, factory: F) -> Result<Self, PluginBundleError>
    where
        F: Fn() -> Result<P, E> + Send + Sync + 'static,
        P: AgentPlugin + 'static,
        E: std::fmt::Display,
    {
        self.ensure_vacant(PluginCapability::Agent)?;
        let plugin_id = self.manifest.id.clone();
        self.agent = Some(Arc::new(move || {
            let plugin = factory().map_err(|error| PluginBundleLoadError::Factory {
                plugin_id: plugin_id.clone(),
                capability: PluginCapability::Agent.as_str(),
                message: error.to_string(),
            })?;
            let plugin: Arc<dyn AgentPlugin> = Arc::new(plugin);
            validate_contribution_id(&plugin_id, PluginCapability::Agent, plugin.id())?;
            Ok(plugin)
        }));
        self.manifest.capabilities.insert(PluginCapability::Agent);
        Ok(self)
    }

    pub fn provider_plugin<F, P>(self, factory: F) -> Result<Self, PluginBundleError>
    where
        F: Fn() -> P + Send + Sync + 'static,
        P: ProviderPlugin + 'static,
    {
        self.try_provider_plugin(move || Ok::<P, std::convert::Infallible>(factory()))
    }

    pub fn try_provider_plugin<F, P, E>(mut self, factory: F) -> Result<Self, PluginBundleError>
    where
        F: Fn() -> Result<P, E> + Send + Sync + 'static,
        P: ProviderPlugin + 'static,
        E: std::fmt::Display,
    {
        self.ensure_vacant(PluginCapability::Provider)?;
        let plugin_id = self.manifest.id.clone();
        self.provider = Some(Arc::new(move || {
            let plugin = factory().map_err(|error| PluginBundleLoadError::Factory {
                plugin_id: plugin_id.clone(),
                capability: PluginCapability::Provider.as_str(),
                message: error.to_string(),
            })?;
            let plugin: Arc<dyn ProviderPlugin> = Arc::new(plugin);
            validate_contribution_id(&plugin_id, PluginCapability::Provider, plugin.id())?;
            Ok(plugin)
        }));
        self.manifest
            .capabilities
            .insert(PluginCapability::Provider);
        Ok(self)
    }

    pub fn session_plugin<F, P>(self, factory: F) -> Result<Self, PluginBundleError>
    where
        F: Fn() -> P + Send + Sync + 'static,
        P: SessionPlugin + 'static,
    {
        self.try_session_plugin(move || Ok::<P, std::convert::Infallible>(factory()))
    }

    pub fn try_session_plugin<F, P, E>(mut self, factory: F) -> Result<Self, PluginBundleError>
    where
        F: Fn() -> Result<P, E> + Send + Sync + 'static,
        P: SessionPlugin + 'static,
        E: std::fmt::Display,
    {
        self.ensure_vacant(PluginCapability::Session)?;
        let plugin_id = self.manifest.id.clone();
        self.session = Some(Arc::new(move || {
            let plugin = factory().map_err(|error| PluginBundleLoadError::Factory {
                plugin_id: plugin_id.clone(),
                capability: PluginCapability::Session.as_str(),
                message: error.to_string(),
            })?;
            let plugin: Arc<dyn SessionPlugin> = Arc::new(plugin);
            validate_contribution_id(&plugin_id, PluginCapability::Session, plugin.id())?;
            Ok(plugin)
        }));
        self.manifest.capabilities.insert(PluginCapability::Session);
        Ok(self)
    }

    fn ensure_vacant(&self, capability: PluginCapability) -> Result<(), PluginBundleError> {
        let occupied = match capability {
            PluginCapability::Agent => self.agent.is_some(),
            PluginCapability::Provider => self.provider.is_some(),
            PluginCapability::Session => self.session.is_some(),
        };
        if occupied {
            return Err(PluginBundleError::DuplicateContribution {
                plugin_id: self.manifest.id.clone(),
                capability: capability.as_str(),
            });
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), PluginBundleError> {
        if self.manifest.id.as_str().trim().is_empty() {
            return Err(PluginBundleError::EmptyId);
        }
        if self.manifest.version.trim().is_empty() {
            return Err(PluginBundleError::EmptyVersion {
                plugin_id: self.manifest.id.clone(),
            });
        }
        if self.manifest.host_api_version != PLUGIN_HOST_API_VERSION {
            return Err(PluginBundleError::IncompatibleHostApi {
                plugin_id: self.manifest.id.clone(),
                requested: self.manifest.host_api_version,
                supported: PLUGIN_HOST_API_VERSION,
            });
        }
        if self.manifest.capabilities.is_empty() {
            return Err(PluginBundleError::EmptyBundle {
                plugin_id: self.manifest.id.clone(),
            });
        }
        Ok(())
    }
}

fn validate_contribution_id(
    plugin_id: &PluginId,
    capability: PluginCapability,
    actual_id: PluginId,
) -> Result<(), PluginBundleLoadError> {
    if &actual_id != plugin_id {
        return Err(PluginBundleLoadError::ContributionIdMismatch {
            plugin_id: plugin_id.clone(),
            capability: capability.as_str(),
            actual_id,
        });
    }
    Ok(())
}

/// Product plugin package set installed in insertion order.
///
/// This is the single installation seam used by product wiring. It validates
/// package metadata and duplicate identities, then adapts each bundle into the
/// existing runtime and session factory seams without inventing another
/// ordering mechanism.
#[derive(Debug, Clone, Default)]
pub struct PluginBundleSet {
    bundles: Vec<PluginBundle>,
}

impl PluginBundleSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bundle(mut self, bundle: PluginBundle) -> Self {
        self.bundles.push(bundle);
        self
    }

    pub fn manifests(&self) -> Vec<&PluginManifest> {
        self.bundles
            .iter()
            .map(|bundle| bundle.manifest())
            .collect()
    }

    pub fn validate(&self) -> Result<(), PluginBundleError> {
        self.validated_bundles().map(|_| ())
    }

    pub fn install_runtime(
        &self,
        mut builder: PiRuntimeBuilder,
    ) -> Result<PiRuntimeBuilder, PluginBundleError> {
        for bundle in self.validated_bundles()? {
            if let Some(factory) = &bundle.agent {
                let factory = Arc::clone(factory);
                builder = builder.try_agent_plugin_arc_factory(move || factory());
            }
            if let Some(factory) = &bundle.provider {
                let factory = Arc::clone(factory);
                builder = builder.try_provider_plugin_arc_factory(move || factory());
            }
        }
        Ok(builder)
    }

    pub fn session_plugins(&self) -> Result<SessionPlugins, PluginBundleError> {
        let mut plugins = SessionPlugins::new();
        for bundle in self.validated_bundles()? {
            if let Some(factory) = &bundle.session {
                let factory = Arc::clone(factory);
                plugins = plugins.try_plugin_arc_factory(move || factory());
            }
        }
        Ok(plugins)
    }

    fn validated_bundles(&self) -> Result<&[PluginBundle], PluginBundleError> {
        let mut seen = HashSet::with_capacity(self.bundles.len());
        for bundle in &self.bundles {
            bundle.validate()?;
            if !seen.insert(bundle.manifest.id.clone()) {
                return Err(PluginBundleError::DuplicateBundle(
                    bundle.manifest.id.clone(),
                ));
            }
        }
        Ok(&self.bundles)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use pi_core::{AgentPlugin, PluginId, RegisterContext};
    use pi_plugin_faux_provider::FauxProviderPlugin;
    use pi_runtime::PiRuntime;

    use super::*;
    use crate::{SessionIdentity, SessionPluginContext, SessionPluginError, SessionStartEvent};

    struct IdPlugin(&'static str);

    #[async_trait]
    impl AgentPlugin for IdPlugin {
        fn id(&self) -> PluginId {
            PluginId::new(self.0)
        }

        fn register(&self, _context: &mut RegisterContext<'_>) -> pi_core::Result<()> {
            Ok(())
        }
    }

    struct IdSessionPlugin;

    #[async_trait]
    impl SessionPlugin for IdSessionPlugin {
        fn id(&self) -> PluginId {
            PluginId::new("session-fixture")
        }

        async fn session_start(
            &self,
            _context: &SessionPluginContext,
            _event: &SessionStartEvent,
        ) -> Result<(), SessionPluginError> {
            Ok(())
        }
    }

    fn agent_bundle(id: &'static str, manifest: PluginManifest) -> PluginBundle {
        PluginBundle::new(manifest)
            .agent_plugin(move || IdPlugin(id))
            .unwrap()
    }

    #[test]
    fn preserves_insertion_order_when_installing_agent_contributions() {
        let bundles = PluginBundleSet::new()
            .bundle(agent_bundle(
                "second",
                PluginManifest::new("second", "1.0.0"),
            ))
            .bundle(agent_bundle(
                "unrelated",
                PluginManifest::new("unrelated", "1.0.0"),
            ))
            .bundle(agent_bundle("first", PluginManifest::new("first", "1.0.0")));

        let manifests = bundles
            .manifests()
            .into_iter()
            .map(|manifest| manifest.id().as_str())
            .collect::<Vec<_>>();
        assert_eq!(manifests, ["second", "unrelated", "first"]);

        let runtime = bundles
            .install_runtime(PiRuntime::builder())
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            runtime.plugin_order(),
            ["second", "unrelated", "first"].map(PluginId::new)
        );
    }

    #[test]
    fn rejects_duplicate_bundle_ids() {
        let duplicate = PluginBundleSet::new()
            .bundle(agent_bundle("same", PluginManifest::new("same", "1.0.0")))
            .bundle(agent_bundle("same", PluginManifest::new("same", "2.0.0")));
        assert!(matches!(
            duplicate.validate().unwrap_err(),
            PluginBundleError::DuplicateBundle(_)
        ));
    }

    #[test]
    fn contribution_id_must_match_bundle_id() {
        let bundle = PluginBundle::new(PluginManifest::new("expected", "1.0.0"))
            .agent_plugin(|| IdPlugin("actual"))
            .unwrap();
        let result = PluginBundleSet::new()
            .bundle(bundle)
            .install_runtime(PiRuntime::builder())
            .unwrap()
            .build();
        let error = match result {
            Ok(_) => panic!("mismatched contribution id must fail runtime construction"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("mismatched id actual"));
    }

    #[tokio::test]
    async fn failed_bundle_factory_reload_keeps_the_previous_generation() {
        let builds = Arc::new(AtomicUsize::new(0));
        let bundle = PluginBundle::new(PluginManifest::new("reloadable", "1.0.0"))
            .try_agent_plugin({
                let builds = Arc::clone(&builds);
                move || {
                    let build = builds.fetch_add(1, Ordering::SeqCst) + 1;
                    if build == 2 {
                        Err("reload fixture failed")
                    } else {
                        Ok(IdPlugin("reloadable"))
                    }
                }
            })
            .unwrap();
        let provider = PluginBundle::new(PluginManifest::new("faux-provider", "1.0.0"))
            .provider_plugin(|| FauxProviderPlugin::scripted([]))
            .unwrap();
        let runtime = PluginBundleSet::new()
            .bundle(provider)
            .bundle(bundle)
            .install_runtime(PiRuntime::builder())
            .unwrap()
            .build()
            .unwrap();

        assert_eq!(runtime.generation(), 1);
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        let error = runtime.reload().await.unwrap_err();
        assert!(error.to_string().contains("reload fixture failed"));
        assert_eq!(runtime.generation(), 1);
        assert_eq!(builds.load(Ordering::SeqCst), 2);

        assert_eq!(runtime.reload().await.unwrap().generation, 2);
        assert_eq!(builds.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn session_contribution_is_factory_backed_across_generations() {
        let builds = Arc::new(AtomicUsize::new(0));
        let bundle = PluginBundle::new(PluginManifest::new("session-fixture", "1.0.0"))
            .session_plugin({
                let builds = Arc::clone(&builds);
                move || {
                    builds.fetch_add(1, Ordering::SeqCst);
                    IdSessionPlugin
                }
            })
            .unwrap();
        let plugins = PluginBundleSet::new()
            .bundle(bundle)
            .session_plugins()
            .unwrap();
        let identity = SessionIdentity {
            id: "session".to_string(),
            path: "/tmp/session.jsonl".into(),
            cwd: "/tmp".into(),
            parent_session_id: None,
        };

        let first = plugins.build(identity).unwrap();
        let second = first.next_generation(&plugins).unwrap();
        assert_eq!(first.plugin_order(), [PluginId::new("session-fixture")]);
        assert_eq!(second.generation(), 2);
        assert_eq!(builds.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn rejects_incompatible_host_api_before_loading_factories() {
        let loads = Arc::new(AtomicUsize::new(0));
        let bundle = PluginBundle::new(
            PluginManifest::new("future", "1.0.0").host_api_version(PLUGIN_HOST_API_VERSION + 1),
        )
        .agent_plugin({
            let loads = Arc::clone(&loads);
            move || {
                loads.fetch_add(1, Ordering::SeqCst);
                IdPlugin("future")
            }
        })
        .unwrap();

        let result = PluginBundleSet::new()
            .bundle(bundle)
            .install_runtime(PiRuntime::builder());
        let error = match result {
            Ok(_) => panic!("incompatible host API must fail bundle installation"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            PluginBundleError::IncompatibleHostApi { .. }
        ));
        assert_eq!(loads.load(Ordering::SeqCst), 0);
    }
}
