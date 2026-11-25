//! A frozen generation policy, not a second selection store.
//!
//! Existing v4 records on the selected branch own desired intent. Runtime state
//! owns only effective names. Product code still owns registry/grant security.
use std::collections::{BTreeMap, HashSet};

use pi_runtime::PiRuntime;

use crate::{AgentSessionOptions, SessionError, ToolSelectionAvailability};

pub(super) struct ToolSelection {
    registered: HashSet<String>,
    withheld: HashSet<String>,
    denied: HashSet<String>,
}

impl ToolSelection {
    pub(super) fn new(runtime: &PiRuntime, options: &AgentSessionOptions) -> Self {
        use ToolSelectionAvailability::{Denied, TemporarilyUnavailable, UnresolvedUntilInventory};
        let registered: HashSet<_> = runtime
            .tool_specs()
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        let mut classifications: BTreeMap<_, _> = options
            .temporarily_unavailable_tools
            .iter()
            .cloned()
            .map(|name| (name, TemporarilyUnavailable))
            .collect();
        classifications.extend(options.tool_selection_availability.clone());
        let mut withheld = HashSet::new();
        let mut denied = HashSet::new();
        for (name, availability) in classifications {
            match availability {
                Denied => {
                    denied.insert(name);
                }
                UnresolvedUntilInventory => {
                    withheld.insert(name);
                }
                TemporarilyUnavailable if !registered.contains(&name) => {
                    withheld.insert(name);
                }
                TemporarilyUnavailable => {}
            }
        }
        Self {
            registered,
            withheld,
            denied,
        }
    }

    /// Merely appearing in a classification never selects a name.
    pub(super) fn reconcile(&self, desired: Vec<String>) -> Vec<String> {
        let mut seen = HashSet::new();
        desired
            .into_iter()
            .filter(|name| {
                !self.denied.contains(name)
                    && (self.registered.contains(name) || self.withheld.contains(name))
                    && seen.insert(name.clone())
            })
            .collect()
    }

    /// Plugin compatibility ignores unrelated unknown names, but explicit
    /// policy denial must still reach the strict setter and fail.
    pub(super) fn plugin_request(&self, requested: Vec<String>) -> Vec<String> {
        requested
            .into_iter()
            .filter(|name| {
                self.registered.contains(name)
                    || self.withheld.contains(name)
                    || self.denied.contains(name)
            })
            .collect()
    }

    /// Projection used for restoration, including in-generation branch checkout.
    pub(super) fn effective(&self, desired: Vec<String>) -> Vec<String> {
        self.reconcile(desired)
            .into_iter()
            .filter(|name| !self.withheld.contains(name))
            .collect()
    }

    /// Other unknowns still reach ordinary runtime validation, preserving the
    /// strict setter and failure-repair errors when no policy is supplied.
    pub(super) fn without_unavailable(&self, desired: Vec<String>) -> Vec<String> {
        desired
            .into_iter()
            .filter(|name| !self.withheld.contains(name) && !self.denied.contains(name))
            .collect()
    }

    pub(super) fn explicit_effective(
        &self,
        desired: &[String],
    ) -> Result<Vec<String>, SessionError> {
        if let Some(name) = desired.iter().find(|name| self.denied.contains(*name)) {
            return Err(SessionError::Runtime(format!(
                "tool selection denied: {name}"
            )));
        }
        Ok(self.without_unavailable(desired.to_vec()))
    }

    pub(super) fn capture(&self, previous: Option<&[String]>, effective: &[String]) -> Vec<String> {
        let mut desired = self.without_unavailable(effective.to_vec());
        for (index, name) in previous.unwrap_or_default().iter().enumerate() {
            if self.withheld.contains(name) && !desired.contains(name) {
                // Keep pending names in their desired positions without changing
                // the relative order of newly discovered executable names.
                desired.insert(index.min(desired.len()), name.clone());
            }
        }
        desired
    }
}

#[cfg(test)]
mod tests;
