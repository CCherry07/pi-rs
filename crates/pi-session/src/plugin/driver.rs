use std::sync::{Arc, Mutex};

use pi_core::{ContextParts, PluginId};

use super::contract::*;

type SessionPluginFactory = Arc<dyn Fn() -> Result<Arc<dyn SessionPlugin>, String> + Send + Sync>;

#[derive(Clone)]
enum SessionPluginSource {
    Pinned(Arc<dyn SessionPlugin>),
    Factory(SessionPluginFactory),
}

impl SessionPluginSource {
    fn load(&self) -> Result<Arc<dyn SessionPlugin>, String> {
        match self {
            Self::Pinned(plugin) => Ok(Arc::clone(plugin)),
            Self::Factory(factory) => factory(),
        }
    }
}

#[derive(Clone, Default)]
pub struct SessionPlugins {
    sources: Vec<SessionPluginSource>,
}

impl SessionPlugins {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn plugin(mut self, plugin: impl SessionPlugin + 'static) -> Self {
        self.sources
            .push(SessionPluginSource::Pinned(Arc::new(plugin)));
        self
    }

    pub fn plugin_arc(mut self, plugin: Arc<dyn SessionPlugin>) -> Self {
        self.sources.push(SessionPluginSource::Pinned(plugin));
        self
    }

    pub fn plugin_factory<F, P>(mut self, factory: F) -> Self
    where
        F: Fn() -> P + Send + Sync + 'static,
        P: SessionPlugin + 'static,
    {
        self.sources
            .push(SessionPluginSource::Factory(Arc::new(move || {
                Ok(Arc::new(factory()))
            })));
        self
    }

    pub fn try_plugin_factory<F, P, E>(mut self, factory: F) -> Self
    where
        F: Fn() -> Result<P, E> + Send + Sync + 'static,
        P: SessionPlugin + 'static,
        E: std::fmt::Display,
    {
        self.sources
            .push(SessionPluginSource::Factory(Arc::new(move || {
                factory()
                    .map(|plugin| Arc::new(plugin) as Arc<dyn SessionPlugin>)
                    .map_err(|error| error.to_string())
            })));
        self
    }

    /// Registers a type-erased, fallible session plugin factory.
    ///
    /// Dynamic plugin adapters use this seam so the session plugin is rebuilt
    /// with every session-plugin generation.
    pub fn try_plugin_arc_factory<F, E>(mut self, factory: F) -> Self
    where
        F: Fn() -> Result<Arc<dyn SessionPlugin>, E> + Send + Sync + 'static,
        E: std::fmt::Display,
    {
        self.sources
            .push(SessionPluginSource::Factory(Arc::new(move || {
                factory().map_err(|error| error.to_string())
            })));
        self
    }

    #[cfg(test)]
    pub(crate) fn build(
        &self,
        identity: SessionIdentity,
    ) -> Result<SessionPluginDriver, SessionPluginError> {
        SessionPluginDriver::build(self, identity, ContextParts::unavailable())
    }

    pub(crate) fn build_with_context(
        &self,
        identity: SessionIdentity,
        context: ContextParts,
    ) -> Result<SessionPluginDriver, SessionPluginError> {
        SessionPluginDriver::build(self, identity, context)
    }
}

struct SessionPluginSlot {
    id: PluginId,
    plugin: Arc<dyn SessionPlugin>,
}

/// Immutable, generation-local session lifecycle hook driver.
pub struct SessionPluginDriver {
    identity: SessionIdentity,
    generation: u64,
    plugins: Vec<SessionPluginSlot>,
    diagnostics: Arc<Mutex<Vec<SessionPluginDiagnostic>>>,
    context: ContextParts,
}

impl SessionPluginDriver {
    fn build(
        sources: &SessionPlugins,
        identity: SessionIdentity,
        context: ContextParts,
    ) -> Result<Self, SessionPluginError> {
        Ok(Self {
            identity,
            generation: 1,
            plugins: load_plugins(sources)?,
            diagnostics: Arc::new(Mutex::new(Vec::new())),
            context,
        })
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn plugin_order(&self) -> Vec<PluginId> {
        self.plugins.iter().map(|slot| slot.id.clone()).collect()
    }

    pub fn identity(&self) -> &SessionIdentity {
        &self.identity
    }

    pub fn diagnostics(&self) -> Vec<SessionPluginDiagnostic> {
        self.diagnostics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn take_diagnostics(&self) -> Vec<SessionPluginDiagnostic> {
        std::mem::take(
            &mut *self
                .diagnostics
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    pub(crate) fn next_generation(
        &self,
        sources: &SessionPlugins,
    ) -> Result<Self, SessionPluginError> {
        let next_id = self.generation().saturating_add(1);
        Ok(Self {
            identity: self.identity.clone(),
            generation: next_id,
            plugins: load_plugins(sources)?,
            diagnostics: Arc::clone(&self.diagnostics),
            context: self.context.clone(),
        })
    }

    pub async fn session_start(&self, event: &SessionStartEvent) {
        for slot in &self.plugins {
            if let Err(error) = slot.plugin.session_start(&self.context(slot), event).await {
                self.record_error(slot, SessionHook::Start, error);
            }
        }
    }

    pub async fn session_info_changed(&self, event: &SessionInfoChangedEvent) {
        for slot in &self.plugins {
            if let Err(error) = slot
                .plugin
                .session_info_changed(&self.context(slot), event)
                .await
            {
                self.record_error(slot, SessionHook::InfoChanged, error);
            }
        }
    }

    pub async fn session_before_switch(
        &self,
        event: &SessionBeforeSwitchEvent,
    ) -> Option<SessionBeforeSwitchResult> {
        let mut result = None;
        for slot in &self.plugins {
            match slot
                .plugin
                .session_before_switch(&self.context(slot), event)
                .await
            {
                Ok(Some(next)) => {
                    let cancelled = next.cancel;
                    result = Some(next);
                    if cancelled {
                        break;
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    self.record_error(slot, SessionHook::BeforeSwitch, error);
                }
            }
        }
        result
    }

    pub async fn session_before_fork(
        &self,
        event: &SessionBeforeForkEvent,
    ) -> Option<SessionBeforeForkResult> {
        let mut result = None;
        for slot in &self.plugins {
            match slot
                .plugin
                .session_before_fork(&self.context(slot), event)
                .await
            {
                Ok(Some(next)) => {
                    let cancelled = next.cancel;
                    result = Some(next);
                    if cancelled {
                        break;
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    self.record_error(slot, SessionHook::BeforeFork, error);
                }
            }
        }
        result
    }

    pub async fn session_before_compact(
        &self,
        event: &SessionBeforeCompactEvent,
    ) -> Option<SessionBeforeCompactResult> {
        let mut result = None;
        for slot in &self.plugins {
            match slot
                .plugin
                .session_before_compact(&self.context(slot), event)
                .await
            {
                Ok(Some(next)) => {
                    let cancelled = next.cancel;
                    result = Some(next);
                    if cancelled {
                        break;
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    self.record_error(slot, SessionHook::BeforeCompact, error);
                }
            }
        }
        result
    }

    pub async fn session_compact(&self, event: &SessionCompactEvent) {
        for slot in &self.plugins {
            if let Err(error) = slot
                .plugin
                .session_compact(&self.context(slot), event)
                .await
            {
                self.record_error(slot, SessionHook::Compact, error);
            }
        }
    }

    pub async fn session_compact_failed(&self, event: &SessionCompactFailedEvent) {
        for slot in &self.plugins {
            if let Err(error) = slot
                .plugin
                .session_compact_failed(&self.context(slot), event)
                .await
            {
                self.record_error(slot, SessionHook::CompactFailed, error);
            }
        }
    }

    pub async fn session_shutdown(&self, event: &SessionShutdownEvent) {
        for slot in &self.plugins {
            if let Err(error) = slot
                .plugin
                .session_shutdown(&self.context(slot), event)
                .await
            {
                self.record_error(slot, SessionHook::Shutdown, error);
            }
        }
    }

    pub async fn session_before_tree(
        &self,
        event: &SessionBeforeTreeEvent,
    ) -> Option<SessionBeforeTreeResult> {
        let mut result = None;
        for slot in &self.plugins {
            match slot
                .plugin
                .session_before_tree(&self.context(slot), event)
                .await
            {
                Ok(Some(next)) => {
                    let cancelled = next.cancel;
                    result = Some(next);
                    if cancelled {
                        break;
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    self.record_error(slot, SessionHook::BeforeTree, error);
                }
            }
        }
        result
    }

    pub async fn session_tree(&self, event: &SessionTreeEvent) {
        for slot in &self.plugins {
            if let Err(error) = slot.plugin.session_tree(&self.context(slot), event).await {
                self.record_error(slot, SessionHook::Tree, error);
            }
        }
    }

    fn context(&self, slot: &SessionPluginSlot) -> SessionPluginContext {
        SessionPluginContext::with_plugin_context(
            slot.id.clone(),
            self.generation,
            self.identity.clone(),
            self.context.clone(),
        )
    }

    fn record_error(&self, slot: &SessionPluginSlot, hook: SessionHook, error: SessionPluginError) {
        self.diagnostics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(SessionPluginDiagnostic {
                plugin_id: slot.id.clone(),
                generation: self.generation,
                hook,
                message: error.to_string(),
            });
    }
}

fn load_plugins(sources: &SessionPlugins) -> Result<Vec<SessionPluginSlot>, SessionPluginError> {
    let mut plugins = Vec::with_capacity(sources.sources.len());
    let mut seen = std::collections::HashSet::new();
    for (index, source) in sources.sources.iter().enumerate() {
        let plugin = source
            .load()
            .map_err(|message| SessionPluginError::Load { index, message })?;
        let id = plugin.id();
        if !seen.insert(id.clone()) {
            return Err(SessionPluginError::DuplicatePlugin(id.to_string()));
        }
        plugins.push(SessionPluginSlot { id, plugin });
    }
    Ok(plugins)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPluginReloadReport {
    pub previous_generation: u64,
    pub generation: u64,
    pub plugin_order: Vec<PluginId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy)]
    enum SwitchBehavior {
        Allow,
        Cancel,
        Error,
    }

    struct SwitchPlugin {
        id: &'static str,
        behavior: SwitchBehavior,
        calls: Arc<Mutex<Vec<&'static str>>>,
    }

    #[pi_session::session_plugin]
    impl SessionPlugin for SwitchPlugin {
        fn id(&self) -> PluginId {
            PluginId::new(self.id)
        }

        async fn session_before_switch(
            &self,
            _context: &SessionPluginContext,
            _event: &SessionBeforeSwitchEvent,
        ) -> Result<Option<SessionBeforeSwitchResult>, SessionPluginError> {
            self.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(self.id);
            match self.behavior {
                SwitchBehavior::Allow => Ok(Some(SessionBeforeSwitchResult { cancel: false })),
                SwitchBehavior::Cancel => Ok(Some(SessionBeforeSwitchResult { cancel: true })),
                SwitchBehavior::Error => Err(SessionPluginError::Failure("fixture".to_string())),
            }
        }
    }

    fn identity() -> SessionIdentity {
        SessionIdentity {
            id: "session".to_string(),
            path: "/tmp/session.jsonl".into(),
            cwd: "/tmp".into(),
            parent_session_id: None,
        }
    }

    fn switch_event() -> SessionBeforeSwitchEvent {
        SessionBeforeSwitchEvent {
            reason: SessionSwitchReason::New,
            target_session_file: None,
        }
    }

    #[tokio::test]
    async fn before_hook_errors_are_isolated_and_later_results_win() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let driver = SessionPlugins::new()
            .plugin(SwitchPlugin {
                id: "first",
                behavior: SwitchBehavior::Allow,
                calls: Arc::clone(&calls),
            })
            .plugin(SwitchPlugin {
                id: "broken",
                behavior: SwitchBehavior::Error,
                calls: Arc::clone(&calls),
            })
            .plugin(SwitchPlugin {
                id: "last",
                behavior: SwitchBehavior::Allow,
                calls: Arc::clone(&calls),
            })
            .build(identity())
            .unwrap();

        let result = driver.session_before_switch(&switch_event()).await.unwrap();
        assert!(!result.cancel);
        assert_eq!(
            *calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec!["first", "broken", "last"]
        );
        assert_eq!(driver.diagnostics().len(), 1);
        assert_eq!(driver.diagnostics()[0].hook, SessionHook::BeforeSwitch);
    }

    #[tokio::test]
    async fn first_before_hook_cancellation_short_circuits_later_plugins() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let driver = SessionPlugins::new()
            .plugin(SwitchPlugin {
                id: "cancel",
                behavior: SwitchBehavior::Cancel,
                calls: Arc::clone(&calls),
            })
            .plugin(SwitchPlugin {
                id: "unreached",
                behavior: SwitchBehavior::Allow,
                calls: Arc::clone(&calls),
            })
            .build(identity())
            .unwrap();

        let result = driver.session_before_switch(&switch_event()).await.unwrap();
        assert!(result.cancel);
        assert_eq!(
            *calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec!["cancel"]
        );
    }
}
