use super::{agent::RegisteredPlugin, session::*};
use crate::{PluginDriver, PluginError};

macro_rules! session_observers {
    ($($method:ident: $hook:ident => $event:ty),* $(,)?) => {
        $(pub async fn $method(&self, context: &SessionDispatchContext, event: &$event) {
            for slot in self.session_plugins(SessionHook::$hook) {
                if let Err(error) = slot.plugin.$method(
                    &self.session_plugin_context(slot, context), event,
                ).await {
                    self.record_session_error(slot, context, SessionHook::$hook, error);
                }
            }
        })*
    };
}

impl PluginDriver {
    session_observers! {
        session_start: Start => SessionStartEvent,
        session_info_changed: InfoChanged => SessionInfoChangedEvent,
        session_compact: Compact => SessionCompactEvent,
        session_compact_failed: CompactFailed => SessionCompactFailedEvent,
        session_shutdown: Shutdown => SessionShutdownEvent,
        session_tree: Tree => SessionTreeEvent,
    }

    pub async fn session_before_switch(
        &self,
        context: &SessionDispatchContext,
        event: &SessionBeforeSwitchEvent,
    ) -> Option<SessionBeforeSwitchResult> {
        let mut result = None;
        for slot in self.session_plugins(SessionHook::BeforeSwitch) {
            match slot
                .plugin
                .session_before_switch(&self.session_plugin_context(slot, context), event)
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
                    self.record_session_error(slot, context, SessionHook::BeforeSwitch, error);
                }
            }
        }
        result
    }

    pub async fn session_before_fork(
        &self,
        context: &SessionDispatchContext,
        event: &SessionBeforeForkEvent,
    ) -> Option<SessionBeforeForkResult> {
        let mut result = None;
        for slot in self.session_plugins(SessionHook::BeforeFork) {
            match slot
                .plugin
                .session_before_fork(&self.session_plugin_context(slot, context), event)
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
                    self.record_session_error(slot, context, SessionHook::BeforeFork, error);
                }
            }
        }
        result
    }

    pub async fn session_before_compact(
        &self,
        context: &SessionDispatchContext,
        event: &SessionBeforeCompactEvent,
    ) -> Option<SessionBeforeCompactResult> {
        let mut result = None;
        for slot in self.session_plugins(SessionHook::BeforeCompact) {
            match slot
                .plugin
                .session_before_compact(&self.session_plugin_context(slot, context), event)
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
                    self.record_session_error(slot, context, SessionHook::BeforeCompact, error);
                }
            }
        }
        result
    }

    pub async fn session_before_tree(
        &self,
        context: &SessionDispatchContext,
        event: &SessionBeforeTreeEvent,
    ) -> Option<SessionBeforeTreeResult> {
        let mut result = None;
        for slot in self.session_plugins(SessionHook::BeforeTree) {
            match slot
                .plugin
                .session_before_tree(&self.session_plugin_context(slot, context), event)
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
                    self.record_session_error(slot, context, SessionHook::BeforeTree, error);
                }
            }
        }
        result
    }

    fn session_plugin_context(
        &self,
        slot: &RegisteredPlugin,
        context: &SessionDispatchContext,
    ) -> SessionPluginContext {
        SessionPluginContext::with_plugin_context(
            slot.id.clone(),
            context.generation,
            context.identity.clone(),
            self.context_parts(),
        )
    }

    fn record_session_error(
        &self,
        slot: &RegisteredPlugin,
        context: &SessionDispatchContext,
        hook: SessionHook,
        error: PluginError,
    ) {
        self.diagnostics.record(
            slot.id.clone(),
            hook,
            Some(context.generation),
            error.to_string(),
        );
    }
}
#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::{AbortHandle, AgentPluginContext, AgentStartEvent, Plugin, PluginId, RunId};

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

    #[pi_plugin::plugin]
    impl Plugin for SwitchPlugin {
        fn id(&self) -> PluginId {
            PluginId::new(self.id)
        }

        async fn session_before_switch(
            &self,
            _context: &SessionPluginContext,
            _event: &SessionBeforeSwitchEvent,
        ) -> Result<Option<SessionBeforeSwitchResult>, PluginError> {
            self.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(self.id);
            match self.behavior {
                SwitchBehavior::Allow => Ok(Some(SessionBeforeSwitchResult { cancel: false })),
                SwitchBehavior::Cancel => Ok(Some(SessionBeforeSwitchResult { cancel: true })),
                SwitchBehavior::Error => Err(PluginError::Failure("fixture".to_string())),
            }
        }
    }

    fn dispatch_context() -> SessionDispatchContext {
        SessionDispatchContext {
            generation: 7,
            identity: SessionIdentity {
                id: "session".to_string(),
                path: "/tmp/session.jsonl".into(),
                cwd: "/tmp".into(),
                parent_session_id: None,
            },
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
        let driver = PluginDriver::new(vec![
            Arc::new(SwitchPlugin {
                id: "first",
                behavior: SwitchBehavior::Allow,
                calls: Arc::clone(&calls),
            }),
            Arc::new(SwitchPlugin {
                id: "broken",
                behavior: SwitchBehavior::Error,
                calls: Arc::clone(&calls),
            }),
            Arc::new(SwitchPlugin {
                id: "last",
                behavior: SwitchBehavior::Allow,
                calls: Arc::clone(&calls),
            }),
        ])
        .unwrap();

        let result = driver
            .session_before_switch(&dispatch_context(), &switch_event())
            .await
            .unwrap();
        assert!(!result.cancel);
        assert_eq!(
            *calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec!["first", "broken", "last"]
        );
        assert_eq!(driver.diagnostics().len(), 1);
        assert_eq!(
            driver.diagnostics()[0].hook,
            SessionHook::BeforeSwitch.into()
        );
    }

    #[tokio::test]
    async fn first_before_hook_cancellation_short_circuits_later_plugins() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let driver = PluginDriver::new(vec![
            Arc::new(SwitchPlugin {
                id: "cancel",
                behavior: SwitchBehavior::Cancel,
                calls: Arc::clone(&calls),
            }),
            Arc::new(SwitchPlugin {
                id: "unreached",
                behavior: SwitchBehavior::Allow,
                calls: Arc::clone(&calls),
            }),
        ])
        .unwrap();

        let result = driver
            .session_before_switch(&dispatch_context(), &switch_event())
            .await
            .unwrap();
        assert!(result.cancel);
        assert_eq!(
            *calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec!["cancel"]
        );
    }

    struct StartPlugin {
        id: &'static str,
        fail: bool,
        calls: Arc<Mutex<Vec<SessionPluginContext>>>,
    }

    #[pi_plugin::plugin]
    impl Plugin for StartPlugin {
        fn id(&self) -> PluginId {
            self.id.into()
        }

        async fn agent_start(
            &self,
            _: AgentPluginContext,
            _: AgentStartEvent,
        ) -> Result<(), PluginError> {
            if self.fail {
                return Err(PluginError::Failure("agent fixture".into()));
            }
            Ok(())
        }

        async fn session_start(
            &self,
            context: &SessionPluginContext,
            _: &SessionStartEvent,
        ) -> Result<(), PluginError> {
            self.calls.lock().unwrap().push(context.clone());
            if self.fail {
                return Err(PluginError::Failure("fixture".into()));
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn dispatch_uses_supplied_session_metadata_and_isolates_observer_failures() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let driver = PluginDriver::new(vec![
            Arc::new(StartPlugin {
                id: "broken",
                fail: true,
                calls: Arc::clone(&calls),
            }),
            Arc::new(StartPlugin {
                id: "observer",
                fail: false,
                calls: Arc::clone(&calls),
            }),
        ])
        .unwrap();
        let first = dispatch_context();
        let second = SessionDispatchContext {
            identity: SessionIdentity {
                id: "another-session".into(),
                path: "/tmp/another/session.jsonl".into(),
                cwd: "/tmp/another".into(),
                parent_session_id: Some(first.identity.id.clone()),
            },
            generation: first.generation,
        };
        let event = SessionStartEvent {
            reason: SessionStartReason::Startup,
            previous_session_file: None,
        };

        driver.session_start(&first, &event).await;
        let (_, signal) = AbortHandle::new();
        driver
            .agent_start(
                &RunId::next(),
                &first.identity.cwd,
                &signal,
                AgentStartEvent,
            )
            .await;
        driver.session_start(&second, &event).await;

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 4);
        for (index, context) in [&first, &second].into_iter().enumerate() {
            for (offset, id) in ["broken", "observer"].into_iter().enumerate() {
                let call = &calls[index * 2 + offset];
                assert_eq!(call.plugin_id().as_str(), id);
                assert_eq!(call.identity(), &context.identity);
                assert_eq!(call.generation(), context.generation);
            }
        }
        let diagnostics = driver.take_diagnostics();
        assert_eq!(diagnostics.len(), 3);
        assert!(
            diagnostics
                .iter()
                .all(|diagnostic| diagnostic.plugin_id.as_str() == "broken")
        );
        assert_eq!(
            diagnostics
                .iter()
                .map(|d| d.hook.as_str())
                .collect::<Vec<_>>(),
            ["session_start", "agent_start", "session_start"]
        );
        assert_eq!(
            diagnostics.iter().map(|d| d.generation).collect::<Vec<_>>(),
            [Some(first.generation), None, Some(second.generation)]
        );
        assert!(driver.diagnostics().is_empty());
    }
}
