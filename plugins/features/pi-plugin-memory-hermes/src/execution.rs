//! Typed invocation state belongs to Hermes, not the Agent/tool runtime.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use pi_core::RunId;
use serde_json::Value;

use crate::consolidation::ConsolidationBudget;

/// The generation's shared tools use this weak directory to find the state
/// owned by the foreground plugin or an invocation-private review plugin.
#[derive(Default)]
pub(crate) struct HermesRuns(Mutex<HashMap<RunId, Weak<HermesRunState>>>);

impl HermesRuns {
    pub(crate) fn get(&self, run_id: Option<&RunId>) -> Option<Arc<HermesRunState>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(run_id?)
            .and_then(Weak::upgrade)
    }

    pub(crate) fn attach(
        self: &Arc<Self>,
        run_id: RunId,
        state: Arc<HermesRunState>,
    ) -> Result<HermesRunLease, &'static str> {
        let mut runs = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if runs.get(&run_id).and_then(Weak::upgrade).is_some() {
            return Err("Hermes execution already has an owner");
        }
        runs.insert(run_id.clone(), Arc::downgrade(&state));
        Ok(HermesRunLease {
            runs: Arc::clone(self),
            run_id,
            state,
        })
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.0.lock().unwrap().len()
    }
}

#[derive(Default)]
pub(crate) struct HermesRunState {
    pub(crate) consolidation: ConsolidationBudget,
    pub(crate) review: Option<ReviewObservations>,
}

impl HermesRunState {
    pub(crate) fn review() -> Self {
        Self {
            review: Some(ReviewObservations::default()),
            ..Self::default()
        }
    }
}

/// Retain only successful reads relevant to the skill read-before-write rule.
/// Foreground history and other reviews never contribute witnesses here.
#[derive(Default)]
pub(crate) struct ReviewObservations(Mutex<Vec<(String, Value)>>);

impl ReviewObservations {
    pub(crate) fn observe_read(&self, name: &str, args: Value) {
        if matches!(name, "read" | "read_file" | "skill_view")
            || (name == "skill_manage"
                && args.get("action").and_then(Value::as_str) == Some("view"))
        {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((name.to_string(), args));
        }
    }

    pub(crate) fn has_read(&self, matches: impl Fn(&str, &Value) -> bool) -> bool {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|(name, args)| matches(name, args))
    }
}

/// Owns a single invocation's state and removes its directory entry on drop,
/// including cancellation, a dropped review future, or a retired generation.
pub(crate) struct HermesRunLease {
    runs: Arc<HermesRuns>,
    pub(crate) run_id: RunId,
    state: Arc<HermesRunState>,
}

impl Drop for HermesRunLease {
    fn drop(&mut self) {
        let mut runs = self
            .runs
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if runs
            .get(&self.run_id)
            .is_some_and(|state| state.ptr_eq(&Arc::downgrade(&self.state)))
        {
            runs.remove(&self.run_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryResult;
    use serde_json::json;

    #[test]
    fn invocation_owners_isolate_budgets_reads_and_lifetimes() {
        let runs = Arc::new(HermesRuns::default());
        let foreground = Arc::new(HermesRunState::default());
        let first = Arc::new(HermesRunState::review());
        let second = Arc::new(HermesRunState::review());
        let foreground_lease = runs.attach(RunId::next(), foreground.clone()).unwrap();
        let first_lease = runs.attach(RunId::next(), first.clone()).unwrap();
        let second_lease = runs.attach(RunId::next(), second.clone()).unwrap();
        for _ in 0..3 {
            assert_eq!(first.consolidation.observe(failure()).done, None);
        }
        assert_eq!(first.consolidation.observe(failure()).done, Some(true));
        assert_eq!(foreground.consolidation.observe(failure()).done, None);
        assert_eq!(second.consolidation.observe(failure()).done, None);
        assert!(foreground.review.is_none());
        first
            .review
            .as_ref()
            .unwrap()
            .observe_read("read", json!({"path":"SKILL.md"}));
        assert!(first.review.as_ref().unwrap().has_read(|_, _| true));
        assert!(!second.review.as_ref().unwrap().has_read(|_, _| true));
        assert!(runs.get(None).is_none());
        assert!(
            runs.attach(first_lease.run_id.clone(), second.clone())
                .is_err()
        );

        let first_id = first_lease.run_id.clone();
        drop(first_lease);
        assert!(runs.get(Some(&first_id)).is_none());
        assert_eq!(runs.len(), 2);
        assert!(runs.get(Some(&foreground_lease.run_id)).is_some());
        assert!(runs.get(Some(&second_lease.run_id)).is_some());
        drop((foreground_lease, second_lease));
        assert_eq!(runs.len(), 0);
    }

    fn failure() -> MemoryResult {
        MemoryResult::consolidation_error("No matching entry")
    }
}
