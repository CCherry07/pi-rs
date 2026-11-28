use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use pi_js_plugin::JsMcpServerMutation;
use pi_session::{LiveRuntimeScopeId, PluginMcpMutation, PluginMcpMutationAccess};

#[derive(Clone, Default)]
pub(crate) struct DynamicMcpRegistry {
    state: Arc<Mutex<DynamicMcpState>>,
}

#[derive(Default)]
struct DynamicMcpState {
    scopes: HashMap<LiveRuntimeScopeId, DynamicMcpScopeState>,
}

#[derive(Default)]
struct DynamicMcpScopeState {
    revision: u64,
    pending: Vec<PendingMutation>,
    committed: Vec<PluginMcpMutation>,
    leases: usize,
}

struct PendingMutation {
    revision: u64,
    mutation: PluginMcpMutation,
}

impl DynamicMcpRegistry {
    pub(crate) fn begin_preparation(&self, scope: LiveRuntimeScopeId) -> DynamicMcpPreparation {
        let revision = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .scopes
            .entry(scope)
            .or_default()
            .revision;
        DynamicMcpPreparation {
            registry: self.clone(),
            scope,
            revision,
            active: true,
        }
    }

    pub(crate) fn candidate(
        &self,
        scope: LiveRuntimeScopeId,
        extension_paths: &[String],
    ) -> DynamicMcpCandidate {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let scope_state = state.scopes.get(&scope);
        let revision = scope_state.map_or(0, |state| state.revision);
        let extension_paths = extension_paths
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let mut overlay = scope_state
            .map(|state| {
                state
                    .committed
                    .iter()
                    .filter(|mutation| extension_paths.contains(mutation_extension_path(mutation)))
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let changed = scope_state.is_some_and(|scope_state| {
            scope_state.pending.iter().any(|pending| {
                pending.revision <= revision
                    && extension_paths.contains(mutation_extension_path(&pending.mutation))
            })
        });
        if let Some(scope_state) = scope_state {
            for pending in scope_state
                .pending
                .iter()
                .filter(|pending| pending.revision <= revision)
                .filter(|pending| {
                    extension_paths.contains(mutation_extension_path(&pending.mutation))
                })
            {
                apply_overlay(&mut overlay, pending.mutation.clone());
            }
        }
        let mutations = overlay.iter().map(js_mutation).collect();
        DynamicMcpCandidate {
            registry: self.clone(),
            scope,
            revision,
            overlay,
            mutations,
            changed,
            settled: false,
        }
    }

    pub(crate) fn scoped_access(
        &self,
        scope: LiveRuntimeScopeId,
    ) -> Arc<dyn PluginMcpMutationAccess> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.scopes.entry(scope).or_default().leases += 1;
        Arc::new(ScopedDynamicMcpAccess {
            registry: self.clone(),
            scope,
        })
    }

    fn stage(&self, scope: LiveRuntimeScopeId, mutation: PluginMcpMutation) -> Result<(), String> {
        validate_mutation(&mutation)?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = state.scopes.entry(scope).or_default();
        state.revision = state.revision.saturating_add(1);
        let revision = state.revision;
        state.pending.push(PendingMutation { revision, mutation });
        Ok(())
    }

    fn has_pending(&self, scope: LiveRuntimeScopeId) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .scopes
            .get(&scope)
            .is_some_and(|state| !state.pending.is_empty())
    }

    fn settle_candidate(
        &self,
        scope: LiveRuntimeScopeId,
        revision: u64,
        overlay: Option<Vec<PluginMcpMutation>>,
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(scope_state) = state.scopes.get_mut(&scope) else {
            return;
        };
        scope_state
            .pending
            .retain(|pending| pending.revision > revision);
        if let Some(overlay) = overlay {
            scope_state.committed = overlay;
        }
        remove_empty_scope(&mut state.scopes, scope);
    }

    fn discard_pending_through(&self, scope: LiveRuntimeScopeId, revision: u64) {
        self.settle_candidate(scope, revision, None);
    }

    fn release_scope(&self, scope: LiveRuntimeScopeId) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(scope_state) = state.scopes.get_mut(&scope) else {
            return;
        };
        scope_state.leases = scope_state.leases.saturating_sub(1);
        if scope_state.leases == 0 {
            state.scopes.remove(&scope);
        }
    }
}

struct ScopedDynamicMcpAccess {
    registry: DynamicMcpRegistry,
    scope: LiveRuntimeScopeId,
}

impl PluginMcpMutationAccess for ScopedDynamicMcpAccess {
    fn stage(&self, mutation: PluginMcpMutation) -> Result<(), String> {
        self.registry.stage(self.scope, mutation)
    }

    fn has_pending(&self) -> bool {
        self.registry.has_pending(self.scope)
    }
}

impl Drop for ScopedDynamicMcpAccess {
    fn drop(&mut self) {
        self.registry.release_scope(self.scope);
    }
}

pub(crate) struct DynamicMcpCandidate {
    registry: DynamicMcpRegistry,
    scope: LiveRuntimeScopeId,
    revision: u64,
    overlay: Vec<PluginMcpMutation>,
    mutations: Vec<JsMcpServerMutation>,
    changed: bool,
    settled: bool,
}

impl DynamicMcpCandidate {
    pub(crate) fn mutations(&self) -> &[JsMcpServerMutation] {
        &self.mutations
    }

    pub(crate) fn has_changes(&self) -> bool {
        self.changed
    }

    pub(crate) fn commit(mut self) {
        self.registry.settle_candidate(
            self.scope,
            self.revision,
            Some(std::mem::take(&mut self.overlay)),
        );
        self.settled = true;
    }

    pub(crate) fn reject(mut self) {
        self.registry
            .discard_pending_through(self.scope, self.revision);
        self.settled = true;
    }
}

impl Drop for DynamicMcpCandidate {
    fn drop(&mut self) {
        if !self.settled {
            self.registry
                .discard_pending_through(self.scope, self.revision);
        }
    }
}

pub(crate) struct DynamicMcpPreparation {
    registry: DynamicMcpRegistry,
    scope: LiveRuntimeScopeId,
    revision: u64,
    active: bool,
}

impl DynamicMcpPreparation {
    pub(crate) fn finish(mut self) {
        self.active = false;
    }
}

impl Drop for DynamicMcpPreparation {
    fn drop(&mut self) {
        if self.active {
            self.registry
                .discard_pending_through(self.scope, self.revision);
        }
    }
}

fn mutation_extension_path(mutation: &PluginMcpMutation) -> &str {
    match mutation {
        PluginMcpMutation::Register { extension_path, .. }
        | PluginMcpMutation::Unregister { extension_path, .. } => extension_path,
    }
}

fn mutation_name(mutation: &PluginMcpMutation) -> &str {
    match mutation {
        PluginMcpMutation::Register { name, .. } | PluginMcpMutation::Unregister { name, .. } => {
            name
        }
    }
}

fn apply_overlay(overlay: &mut Vec<PluginMcpMutation>, mutation: PluginMcpMutation) {
    let position = overlay.iter().position(|existing| {
        mutation_extension_path(existing) == mutation_extension_path(&mutation)
            && mutation_name(existing) == mutation_name(&mutation)
    });
    match (position, &mutation) {
        (Some(position), PluginMcpMutation::Register { .. })
            if matches!(overlay[position], PluginMcpMutation::Unregister { .. }) =>
        {
            overlay.remove(position);
            overlay.push(mutation);
        }
        (Some(position), _) => overlay[position] = mutation,
        (None, _) => overlay.push(mutation),
    }
}

fn js_mutation(mutation: &PluginMcpMutation) -> JsMcpServerMutation {
    match mutation {
        PluginMcpMutation::Register {
            extension_path,
            name,
            config,
        } => JsMcpServerMutation::Register {
            extension_path: extension_path.clone(),
            name: name.clone(),
            config: config.clone(),
        },
        PluginMcpMutation::Unregister {
            extension_path,
            name,
        } => JsMcpServerMutation::Unregister {
            extension_path: extension_path.clone(),
            name: name.clone(),
        },
    }
}

fn validate_mutation(mutation: &PluginMcpMutation) -> Result<(), String> {
    let extension_path = mutation_extension_path(mutation);
    let name = mutation_name(mutation);
    if extension_path.trim().is_empty() {
        return Err("extension MCP mutation needs an extension path".to_string());
    }
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(format!("invalid MCP server name {name:?}"));
    }
    if matches!(
        mutation,
        PluginMcpMutation::Register { config, .. } if !config.is_object()
    ) {
        return Err(format!("MCP server {name:?} config must be an object"));
    }
    Ok(())
}

fn remove_empty_scope(
    scopes: &mut HashMap<LiveRuntimeScopeId, DynamicMcpScopeState>,
    scope: LiveRuntimeScopeId,
) {
    if scopes.get(&scope).is_some_and(|state| {
        state.leases == 0 && state.pending.is_empty() && state.committed.is_empty()
    }) {
        scopes.remove(&scope);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn register(path: &str, name: &str) -> PluginMcpMutation {
        PluginMcpMutation::Register {
            extension_path: path.to_string(),
            name: name.to_string(),
            config: serde_json::json!({"command":"fixture"}),
        }
    }

    fn unregister(path: &str, name: &str) -> PluginMcpMutation {
        PluginMcpMutation::Unregister {
            extension_path: path.to_string(),
            name: name.to_string(),
        }
    }

    #[test]
    fn committed_overlay_persists_across_reload_and_isolated_scopes() {
        let registry = DynamicMcpRegistry::default();
        let scope = LiveRuntimeScopeId::new();
        let other_scope = LiveRuntimeScopeId::new();
        let access = registry.scoped_access(scope);
        access
            .stage(register("/extensions/owner.ts", "docs"))
            .unwrap();
        let initial = registry.candidate(scope, &["/extensions/owner.ts".to_string()]);
        assert!(initial.has_changes());
        initial.commit();

        let reload = registry.candidate(scope, &["/extensions/owner.ts".to_string()]);
        assert!(!reload.has_changes());
        assert!(matches!(
            reload.mutations(),
            [JsMcpServerMutation::Register { name, .. }] if name == "docs"
        ));
        let isolated = registry.candidate(other_scope, &["/extensions/owner.ts".to_string()]);
        assert!(isolated.mutations().is_empty());
        isolated.reject();
        reload.commit();

        access
            .stage(unregister("/extensions/owner.ts", "docs"))
            .unwrap();
        registry
            .candidate(scope, &["/extensions/owner.ts".to_string()])
            .commit();
        let tombstone = registry.candidate(scope, &["/extensions/owner.ts".to_string()]);
        assert!(matches!(
            tombstone.mutations(),
            [JsMcpServerMutation::Unregister { name, .. }] if name == "docs"
        ));
        tombstone.commit();
    }

    #[test]
    fn replacement_keeps_order_while_remove_then_readd_moves_to_the_end() {
        let registry = DynamicMcpRegistry::default();
        let scope = LiveRuntimeScopeId::new();
        let access = registry.scoped_access(scope);
        access
            .stage(register("/extensions/owner.ts", "first"))
            .unwrap();
        access
            .stage(register("/extensions/owner.ts", "second"))
            .unwrap();
        registry
            .candidate(scope, &["/extensions/owner.ts".to_string()])
            .commit();

        access
            .stage(register("/extensions/owner.ts", "first"))
            .unwrap();
        let replaced = registry.candidate(scope, &["/extensions/owner.ts".to_string()]);
        assert!(matches!(
            replaced.mutations(),
            [
                JsMcpServerMutation::Register { name: first, .. },
                JsMcpServerMutation::Register { name: second, .. },
            ] if first == "first" && second == "second"
        ));
        replaced.commit();

        access
            .stage(unregister("/extensions/owner.ts", "first"))
            .unwrap();
        registry
            .candidate(scope, &["/extensions/owner.ts".to_string()])
            .commit();
        access
            .stage(register("/extensions/owner.ts", "first"))
            .unwrap();
        let readded = registry.candidate(scope, &["/extensions/owner.ts".to_string()]);
        assert!(matches!(
            readded.mutations(),
            [
                JsMcpServerMutation::Register { name: second, .. },
                JsMcpServerMutation::Register { name: first, .. },
            ] if first == "first" && second == "second"
        ));
    }

    #[test]
    fn candidate_preserves_order_filters_stale_owners_and_commits_once() {
        let registry = DynamicMcpRegistry::default();
        let scope = LiveRuntimeScopeId::new();
        let access = registry.scoped_access(scope);
        access
            .stage(register("/extensions/owner.ts", "docs"))
            .unwrap();
        access
            .stage(unregister("/extensions/missing.ts", "old"))
            .unwrap();

        let candidate = registry.candidate(scope, &["/extensions/owner.ts".to_string()]);
        assert_eq!(candidate.mutations().len(), 1);
        assert!(matches!(
            &candidate.mutations()[0],
            JsMcpServerMutation::Register { name, .. } if name == "docs"
        ));
        candidate.commit();
        assert!(!access.has_pending());
    }

    #[test]
    fn failed_preparation_rejects_only_mutations_visible_at_its_start() {
        let registry = DynamicMcpRegistry::default();
        let scope = LiveRuntimeScopeId::new();
        let access = registry.scoped_access(scope);
        access
            .stage(unregister("/extensions/owner.ts", "old"))
            .unwrap();
        let preparation = registry.begin_preparation(scope);
        access
            .stage(register("/extensions/owner.ts", "new"))
            .unwrap();
        drop(preparation);

        let candidate = registry.candidate(scope, &["/extensions/owner.ts".to_string()]);
        assert_eq!(candidate.mutations().len(), 1);
        assert!(matches!(
            &candidate.mutations()[0],
            JsMcpServerMutation::Register { name, .. } if name == "new"
        ));
    }

    #[test]
    fn dropping_prepared_candidate_preserves_only_later_mutations() {
        let registry = DynamicMcpRegistry::default();
        let scope = LiveRuntimeScopeId::new();
        let access = registry.scoped_access(scope);
        access
            .stage(register("/extensions/owner.ts", "baseline"))
            .unwrap();
        registry
            .candidate(scope, &["/extensions/owner.ts".to_string()])
            .commit();

        access
            .stage(register("/extensions/owner.ts", "cancelled"))
            .unwrap();
        let cancelled = registry.candidate(scope, &["/extensions/owner.ts".to_string()]);
        access
            .stage(register("/extensions/owner.ts", "later"))
            .unwrap();
        drop(cancelled);

        let replacement = registry.candidate(scope, &["/extensions/owner.ts".to_string()]);
        assert!(replacement.has_changes());
        assert!(matches!(
            replacement.mutations(),
            [
                JsMcpServerMutation::Register { name: baseline, .. },
                JsMcpServerMutation::Register { name: later, .. },
            ] if baseline == "baseline" && later == "later"
        ));
        replacement.commit();
        assert!(!access.has_pending());
    }

    #[test]
    fn scope_close_before_candidate_commit_cannot_resurrect_overlay() {
        let registry = DynamicMcpRegistry::default();
        let scope = LiveRuntimeScopeId::new();
        let access = registry.scoped_access(scope);
        access
            .stage(register("/extensions/owner.ts", "docs"))
            .unwrap();
        let candidate = registry.candidate(scope, &["/extensions/owner.ts".to_string()]);

        drop(access);
        candidate.commit();

        let replacement_access = registry.scoped_access(scope);
        let replacement = registry.candidate(scope, &["/extensions/owner.ts".to_string()]);
        assert!(!replacement.has_changes());
        assert!(replacement.mutations().is_empty());
        replacement.commit();
        drop(replacement_access);
    }

    #[test]
    fn dropping_last_scope_access_discards_runtime_overlay() {
        let registry = DynamicMcpRegistry::default();
        let scope = LiveRuntimeScopeId::new();
        let access = registry.scoped_access(scope);
        access
            .stage(register("/extensions/owner.ts", "docs"))
            .unwrap();
        registry
            .candidate(scope, &["/extensions/owner.ts".to_string()])
            .commit();
        drop(access);

        let candidate = registry.candidate(scope, &["/extensions/owner.ts".to_string()]);
        assert!(candidate.mutations().is_empty());
    }

    #[test]
    fn mutation_debug_output_redacts_config() {
        let mutation = PluginMcpMutation::Register {
            extension_path: "/extensions/owner.ts".to_string(),
            name: "docs".to_string(),
            config: serde_json::json!({"headers":{"Authorization":"secret-marker"}}),
        };
        let debug = format!("{mutation:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("secret-marker"));
    }
}
