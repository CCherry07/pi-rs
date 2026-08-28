use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use pi_core::ProviderId;
use pi_js_plugin::{
    ExtensionProviderMutation, ExtensionProviderMutationAccess, JsProviderRegistration,
};
use serde_json::{Map, Value};

#[derive(Clone, Default)]
pub(crate) struct DynamicProviderOverlay {
    state: Arc<Mutex<DynamicProviderState>>,
}

#[derive(Default)]
struct DynamicProviderState {
    next_revision: u64,
    committed: BTreeMap<String, Value>,
    pending: Vec<PendingMutation>,
}

struct PendingMutation {
    revision: u64,
    mutation: ExtensionProviderMutation,
}

pub(crate) struct DynamicProviderCandidate {
    revision: u64,
    providers: BTreeMap<String, Value>,
}

pub(crate) struct DynamicProviderPreparation {
    overlay: DynamicProviderOverlay,
    revision: u64,
    finished: bool,
}

impl DynamicProviderOverlay {
    pub(crate) fn begin_preparation(&self) -> DynamicProviderPreparation {
        let revision = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .next_revision;
        DynamicProviderPreparation {
            overlay: self.clone(),
            revision,
            finished: false,
        }
    }

    pub(crate) fn candidate(
        &self,
        registrations: &[JsProviderRegistration],
    ) -> Result<DynamicProviderCandidate, String> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let revision = state.next_revision;
        let mut providers = state.committed.clone();

        // Re-evaluated extension factories behave like Pi's load-time queue.
        // A runtime mutation that caused this rebuild remains the top layer.
        for registration in registrations {
            apply_mutation(
                &mut providers,
                &ExtensionProviderMutation::Register {
                    name: registration.name.clone(),
                    config: registration.config.clone(),
                },
            )?;
        }
        for pending in state
            .pending
            .iter()
            .filter(|pending| pending.revision <= revision)
        {
            apply_mutation(&mut providers, &pending.mutation)?;
        }

        Ok(DynamicProviderCandidate {
            revision,
            providers,
        })
    }

    pub(crate) fn commit(&self, candidate: DynamicProviderCandidate) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.committed = candidate.providers;
        state
            .pending
            .retain(|pending| pending.revision > candidate.revision);
    }

    pub(crate) fn reject(&self, candidate: &DynamicProviderCandidate) {
        self.reject_through(candidate.revision);
    }

    fn reject_through(&self, revision: u64) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending
            .retain(|pending| pending.revision > revision);
    }
}

impl DynamicProviderPreparation {
    pub(crate) fn finish(&mut self) {
        self.finished = true;
    }
}

impl Drop for DynamicProviderPreparation {
    fn drop(&mut self) {
        if !self.finished {
            self.overlay.reject_through(self.revision);
        }
    }
}

impl DynamicProviderCandidate {
    pub(crate) fn provider_configs(&self) -> impl Iterator<Item = (ProviderId, Value)> + '_ {
        self.providers
            .iter()
            .map(|(name, config)| (ProviderId::new(name), config.clone()))
    }
}

impl ExtensionProviderMutationAccess for DynamicProviderOverlay {
    fn stage(&self, mutation: ExtensionProviderMutation) -> Result<(), String> {
        validate_mutation(&mutation)?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.next_revision = state.next_revision.saturating_add(1);
        let revision = state.next_revision;
        state.pending.push(PendingMutation { revision, mutation });
        Ok(())
    }

    fn has_pending(&self) -> bool {
        !self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending
            .is_empty()
    }
}

fn validate_mutation(mutation: &ExtensionProviderMutation) -> Result<(), String> {
    match mutation {
        ExtensionProviderMutation::Register { name, config } => {
            if name.trim().is_empty() {
                return Err("provider name must not be empty".to_string());
            }
            if !config.is_object() {
                return Err(format!("provider {name} config must be an object"));
            }
        }
        ExtensionProviderMutation::Unregister { name } if name.trim().is_empty() => {
            return Err("provider name must not be empty".to_string());
        }
        ExtensionProviderMutation::Unregister { .. } => {}
    }
    Ok(())
}

fn apply_mutation(
    providers: &mut BTreeMap<String, Value>,
    mutation: &ExtensionProviderMutation,
) -> Result<(), String> {
    validate_mutation(mutation)?;
    match mutation {
        ExtensionProviderMutation::Register { name, config } => {
            let overlay = config
                .as_object()
                .expect("validated provider config must be an object");
            let target = providers
                .entry(name.clone())
                .or_insert_with(|| Value::Object(Map::new()))
                .as_object_mut()
                .ok_or_else(|| format!("provider {name} config must be an object"))?;
            target.extend(overlay.clone());
        }
        ExtensionProviderMutation::Unregister { name } => {
            providers.remove(name);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registration(config: Value) -> JsProviderRegistration {
        JsProviderRegistration {
            plugin_id: "js:0:fixture.ts".to_string(),
            path: "/fixture.ts".to_string(),
            name: "proxy".to_string(),
            config,
        }
    }

    fn proxy(candidate: &DynamicProviderCandidate) -> Option<&Value> {
        candidate.providers.get("proxy")
    }

    #[test]
    fn runtime_mutations_are_transactional_and_load_time_registration_restores_after_unregister() {
        let overlay = DynamicProviderOverlay::default();
        let initial = [registration(serde_json::json!({
            "baseUrl": "https://load.example/v1",
            "api": "openai-responses"
        }))];
        overlay.commit(overlay.candidate(&initial).unwrap());

        overlay
            .stage(ExtensionProviderMutation::Register {
                name: "proxy".to_string(),
                config: serde_json::json!({"baseUrl": "https://runtime.example/v1"}),
            })
            .unwrap();
        let rejected = overlay.candidate(&initial).unwrap();
        assert_eq!(
            proxy(&rejected).unwrap()["baseUrl"],
            "https://runtime.example/v1"
        );
        overlay.reject(&rejected);
        assert!(!overlay.has_pending());
        let unchanged = overlay.candidate(&[]).unwrap();
        assert_eq!(
            proxy(&unchanged).unwrap()["baseUrl"],
            "https://load.example/v1"
        );

        overlay
            .stage(ExtensionProviderMutation::Unregister {
                name: "proxy".to_string(),
            })
            .unwrap();
        let removed = overlay.candidate(&initial).unwrap();
        assert!(proxy(&removed).is_none());
        overlay.commit(removed);

        let restored = overlay.candidate(&initial).unwrap();
        assert_eq!(
            proxy(&restored).unwrap()["baseUrl"],
            "https://load.example/v1"
        );
    }

    #[test]
    fn committing_one_revision_preserves_mutations_staged_during_preparation() {
        let overlay = DynamicProviderOverlay::default();
        overlay
            .stage(ExtensionProviderMutation::Register {
                name: "one".to_string(),
                config: serde_json::json!({"baseUrl": "https://one.example"}),
            })
            .unwrap();
        let first = overlay.candidate(&[]).unwrap();
        overlay
            .stage(ExtensionProviderMutation::Register {
                name: "two".to_string(),
                config: serde_json::json!({"baseUrl": "https://two.example"}),
            })
            .unwrap();
        overlay.commit(first);

        assert!(overlay.has_pending());
        let second = overlay.candidate(&[]).unwrap();
        assert!(second.providers.contains_key("one"));
        assert!(second.providers.contains_key("two"));
    }

    #[test]
    fn failed_preparation_rejects_only_mutations_visible_when_it_started() {
        let overlay = DynamicProviderOverlay::default();
        overlay
            .stage(ExtensionProviderMutation::Register {
                name: "one".to_string(),
                config: serde_json::json!({"baseUrl": "https://one.example"}),
            })
            .unwrap();
        let attempt = overlay.begin_preparation();
        overlay
            .stage(ExtensionProviderMutation::Register {
                name: "two".to_string(),
                config: serde_json::json!({"baseUrl": "https://two.example"}),
            })
            .unwrap();

        drop(attempt);

        let candidate = overlay.candidate(&[]).unwrap();
        assert!(!candidate.providers.contains_key("one"));
        assert!(candidate.providers.contains_key("two"));
    }
}
