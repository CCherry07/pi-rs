use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use uuid::Uuid;

use crate::profiles::SubagentProfile;

pub(crate) const DEFAULT_MAX_DEPTH: usize = 1;
const DEFAULT_MAX_SPAWNS_PER_ROOT: usize = 64;
const DEFAULT_MAX_ACTIVE_RUNS: usize = 20;
const MARKER_PREFIX: &str = "<!-- pi-rs-subagent-run:";
const MARKER_SUFFIX: &str = " -->";

#[derive(Clone)]
pub struct SubagentRuntime {
    inner: Arc<RuntimeInner>,
}

struct RuntimeInner {
    limits: RuntimeLimits,
    state: Mutex<RuntimeState>,
}

#[derive(Debug, Clone, Copy)]
struct RuntimeLimits {
    max_depth: usize,
    max_spawns_per_root: usize,
    max_active_runs: usize,
}

#[derive(Default)]
struct RuntimeState {
    lineages: HashMap<String, Lineage>,
    roots: HashMap<String, RootBudget>,
    runs: HashMap<String, RunRecord>,
}

#[derive(Clone)]
struct Lineage {
    root_session_id: String,
    depth: usize,
    max_depth: usize,
    profile_name: Option<String>,
    allow_nested_subagents: bool,
}

#[derive(Default)]
struct RootBudget {
    spawns: usize,
}

struct RunRecord {
    parent_session_id: String,
    root_session_id: String,
    depth: usize,
    max_depth: usize,
    profile: SubagentProfile,
    child_session_id: Option<String>,
    warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum LaunchError {
    #[error("subagent nesting limit reached (depth {depth}, maximum {maximum})")]
    Depth { depth: usize, maximum: usize },
    #[error("subagent spawn budget reached for this root session ({used}/{maximum})")]
    SpawnBudget { used: usize, maximum: usize },
    #[error("subagent concurrency limit reached ({active}/{maximum} active)")]
    Concurrency { active: usize, maximum: usize },
    #[error("subagent profile {profile:?} does not authorize nested delegation")]
    NestedDelegationDisabled { profile: String },
    #[error("unknown subagent run: {0}")]
    UnknownRun(String),
    #[error("subagent run {run_id} is already bound to another child session")]
    AlreadyBound { run_id: String },
}

#[derive(Debug)]
pub(crate) struct LaunchTicket {
    run_id: String,
    profile: SubagentProfile,
    depth: usize,
}

impl LaunchTicket {
    pub(crate) fn run_id(&self) -> &str {
        &self.run_id
    }

    pub(crate) fn depth(&self) -> usize {
        self.depth
    }

    pub(crate) fn child_prompt(&self, task: &str) -> String {
        format!(
            "{marker}\nYou are the `{profile}` delegated subagent. Complete the task below and return the result to the parent session.\n\nTask:\n{task}",
            marker = marker(&self.run_id),
            profile = self.profile.name,
        )
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ChildAssignment {
    pub(crate) profile: SubagentProfile,
    pub(crate) depth: usize,
    pub(crate) max_depth: usize,
}

impl Default for SubagentRuntime {
    fn default() -> Self {
        Self::with_limits(RuntimeLimits {
            max_depth: DEFAULT_MAX_DEPTH,
            max_spawns_per_root: DEFAULT_MAX_SPAWNS_PER_ROOT,
            max_active_runs: DEFAULT_MAX_ACTIVE_RUNS,
        })
    }
}

impl SubagentRuntime {
    fn with_limits(limits: RuntimeLimits) -> Self {
        Self {
            inner: Arc::new(RuntimeInner {
                limits,
                state: Mutex::new(RuntimeState::default()),
            }),
        }
    }

    pub(crate) fn default_max_depth(&self) -> usize {
        self.inner.limits.max_depth
    }

    #[cfg(test)]
    pub(crate) fn begin_launch(
        &self,
        parent_session_id: &str,
        profile: SubagentProfile,
    ) -> Result<LaunchTicket, LaunchError> {
        self.begin_launch_with_max_depth(parent_session_id, profile, self.inner.limits.max_depth)
    }

    pub(crate) fn begin_launch_with_max_depth(
        &self,
        parent_session_id: &str,
        profile: SubagentProfile,
        configured_max_depth: usize,
    ) -> Result<LaunchTicket, LaunchError> {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let lineage = match state.lineages.get(parent_session_id).cloned() {
            Some(mut lineage)
                if lineage.depth == 0 && lineage.root_session_id == parent_session_id =>
            {
                lineage.max_depth = configured_max_depth;
                state
                    .lineages
                    .insert(parent_session_id.to_string(), lineage.clone());
                lineage
            }
            Some(lineage) => lineage,
            None => {
                let lineage = Lineage {
                    root_session_id: parent_session_id.to_string(),
                    depth: 0,
                    max_depth: configured_max_depth,
                    profile_name: None,
                    allow_nested_subagents: true,
                };
                state
                    .lineages
                    .insert(parent_session_id.to_string(), lineage.clone());
                lineage
            }
        };
        if !lineage.allow_nested_subagents {
            return Err(LaunchError::NestedDelegationDisabled {
                profile: lineage
                    .profile_name
                    .unwrap_or_else(|| "unknown".to_string()),
            });
        }
        if lineage.depth >= lineage.max_depth {
            return Err(LaunchError::Depth {
                depth: lineage.depth,
                maximum: lineage.max_depth,
            });
        }
        let active = state.runs.len();
        if active >= self.inner.limits.max_active_runs {
            return Err(LaunchError::Concurrency {
                active,
                maximum: self.inner.limits.max_active_runs,
            });
        }
        let budget = state
            .roots
            .entry(lineage.root_session_id.clone())
            .or_default();
        if budget.spawns >= self.inner.limits.max_spawns_per_root {
            return Err(LaunchError::SpawnBudget {
                used: budget.spawns,
                maximum: self.inner.limits.max_spawns_per_root,
            });
        }
        budget.spawns += 1;

        let run_id = Uuid::now_v7().to_string();
        let depth = lineage.depth + 1;
        let max_depth = profile
            .max_subagent_depth
            .map_or(lineage.max_depth, |maximum| lineage.max_depth.min(maximum));
        state.runs.insert(
            run_id.clone(),
            RunRecord {
                parent_session_id: parent_session_id.to_string(),
                root_session_id: lineage.root_session_id,
                depth,
                max_depth,
                profile: profile.clone(),
                child_session_id: None,
                warnings: Vec::new(),
            },
        );
        Ok(LaunchTicket {
            run_id,
            profile,
            depth,
        })
    }

    pub(crate) fn bind_child(
        &self,
        run_id: &str,
        child_session_id: &str,
    ) -> Result<ChildAssignment, LaunchError> {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let run = state
            .runs
            .get_mut(run_id)
            .ok_or_else(|| LaunchError::UnknownRun(run_id.to_string()))?;
        if run
            .child_session_id
            .as_deref()
            .is_some_and(|bound| bound != child_session_id)
        {
            return Err(LaunchError::AlreadyBound {
                run_id: run_id.to_string(),
            });
        }
        run.child_session_id = Some(child_session_id.to_string());
        let assignment = ChildAssignment {
            profile: run.profile.clone(),
            depth: run.depth,
            max_depth: run.max_depth,
        };
        let lineage = Lineage {
            root_session_id: run.root_session_id.clone(),
            depth: run.depth,
            max_depth: run.max_depth,
            profile_name: Some(run.profile.name.clone()),
            allow_nested_subagents: run.profile.allow_nested_subagents,
        };
        state.lineages.insert(child_session_id.to_string(), lineage);
        Ok(assignment)
    }

    pub(crate) fn profile_for_run(&self, run_id: &str) -> Option<SubagentProfile> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .runs
            .get(run_id)
            .map(|run| run.profile.clone())
    }

    pub(crate) fn record_warnings(&self, run_id: &str, warnings: Vec<String>) {
        if let Some(run) = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .runs
            .get_mut(run_id)
        {
            run.warnings = warnings;
        }
    }

    pub(crate) fn warnings(&self, run_id: &str) -> Vec<String> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .runs
            .get(run_id)
            .map(|run| run.warnings.clone())
            .unwrap_or_default()
    }

    pub(crate) fn cancel_unlaunched(&self, run_id: &str) {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(run) = state.runs.remove(run_id) else {
            return;
        };
        if let Some(child_session_id) = run.child_session_id {
            state.lineages.remove(&child_session_id);
            return;
        }
        if let Some(budget) = state.roots.get_mut(&run.root_session_id) {
            budget.spawns = budget.spawns.saturating_sub(1);
        }
    }

    pub(crate) fn finish(&self, run_id: &str) {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(run) = state.runs.remove(run_id)
            && let Some(child_session_id) = run.child_session_id
        {
            state.lineages.remove(&child_session_id);
        }
    }

    pub(crate) fn forget_session(&self, session_id: &str) {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let lineage = state.lineages.remove(session_id);
        let closing_root = lineage
            .as_ref()
            .is_some_and(|lineage| lineage.root_session_id == session_id && lineage.depth == 0);
        let run_ids = state
            .runs
            .iter()
            .filter(|(_, run)| {
                if closing_root {
                    run.root_session_id == session_id
                } else {
                    run.parent_session_id == session_id
                        || run.child_session_id.as_deref() == Some(session_id)
                }
            })
            .map(|(run_id, _)| run_id.clone())
            .collect::<Vec<_>>();
        for run_id in run_ids {
            if let Some(run) = state.runs.remove(&run_id)
                && let Some(child_session_id) = run.child_session_id
            {
                state.lineages.remove(&child_session_id);
            }
        }
        if closing_root {
            state.roots.remove(session_id);
            state
                .lineages
                .retain(|_, lineage| lineage.root_session_id != session_id);
        }
    }
}

pub(crate) fn run_marker(text: &str) -> Option<&str> {
    let first_line = text.lines().next()?;
    first_line
        .strip_prefix(MARKER_PREFIX)?
        .strip_suffix(MARKER_SUFFIX)
        .filter(|run_id| !run_id.is_empty())
}

fn marker(run_id: &str) -> String {
    format!("{MARKER_PREFIX}{run_id}{MARKER_SUFFIX}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::builtin_profile;

    fn runtime(max_depth: usize, max_spawns: usize, max_active: usize) -> SubagentRuntime {
        SubagentRuntime::with_limits(RuntimeLimits {
            max_depth,
            max_spawns_per_root: max_spawns,
            max_active_runs: max_active,
        })
    }

    #[test]
    fn lineage_allows_bounded_recursive_children() {
        let runtime = runtime(2, 8, 8);
        let first = runtime
            .begin_launch("root", builtin_profile("delegate"))
            .unwrap();
        runtime.bind_child(first.run_id(), "child").unwrap();
        let second = runtime
            .begin_launch("child", builtin_profile("scout"))
            .unwrap();
        assert_eq!(second.depth(), 2);
        runtime.bind_child(second.run_id(), "grandchild").unwrap();

        assert_eq!(
            runtime
                .begin_launch("grandchild", builtin_profile("oracle"))
                .unwrap_err(),
            LaunchError::Depth {
                depth: 2,
                maximum: 2
            }
        );

        runtime.finish(second.run_id());
        runtime.finish(first.run_id());
    }

    #[test]
    fn default_runtime_allows_one_child_depth_and_rejects_the_second() {
        let runtime = SubagentRuntime::default();
        let ticket = runtime
            .begin_launch("root", builtin_profile("delegate"))
            .unwrap();
        assert_eq!(ticket.depth(), 1);
        runtime.bind_child(ticket.run_id(), "child").unwrap();

        assert_eq!(
            runtime
                .begin_launch("child", builtin_profile("delegate"))
                .unwrap_err(),
            LaunchError::Depth {
                depth: 1,
                maximum: 1
            }
        );
        runtime.finish(ticket.run_id());
    }

    #[test]
    fn spawn_budget_is_cumulative_but_failed_launches_are_rolled_back() {
        let runtime = runtime(3, 1, 3);
        let failed = runtime
            .begin_launch("root", builtin_profile("delegate"))
            .unwrap();
        runtime.cancel_unlaunched(failed.run_id());
        let completed = runtime
            .begin_launch("root", builtin_profile("delegate"))
            .unwrap();
        runtime.finish(completed.run_id());

        assert_eq!(
            runtime
                .begin_launch("root", builtin_profile("delegate"))
                .unwrap_err(),
            LaunchError::SpawnBudget {
                used: 1,
                maximum: 1
            }
        );
    }

    #[test]
    fn active_run_limit_is_independent_from_cumulative_budget() {
        let runtime = runtime(3, 10, 1);
        let active = runtime
            .begin_launch("root", builtin_profile("delegate"))
            .unwrap();
        assert_eq!(
            runtime
                .begin_launch("root", builtin_profile("scout"))
                .unwrap_err(),
            LaunchError::Concurrency {
                active: 1,
                maximum: 1
            }
        );
        runtime.finish(active.run_id());
        runtime
            .begin_launch("root", builtin_profile("scout"))
            .unwrap();
    }

    #[test]
    fn marker_round_trips_only_from_the_first_line() {
        let ticket = runtime(3, 10, 10)
            .begin_launch("root", builtin_profile("reviewer"))
            .unwrap();
        let prompt = ticket.child_prompt("Review this change");
        assert_eq!(run_marker(&prompt), Some(ticket.run_id()));
        assert_eq!(
            run_marker(&format!("task\n{}", marker(ticket.run_id()))),
            None
        );
    }

    #[test]
    fn closing_a_root_releases_its_cumulative_state_but_completion_does_not() {
        let runtime = runtime(3, 1, 3);
        let completed = runtime
            .begin_launch("root", builtin_profile("delegate"))
            .unwrap();
        runtime.finish(completed.run_id());
        assert!(matches!(
            runtime.begin_launch("root", builtin_profile("delegate")),
            Err(LaunchError::SpawnBudget { .. })
        ));

        runtime.forget_session("root");
        runtime
            .begin_launch("root", builtin_profile("delegate"))
            .unwrap();
    }

    #[test]
    fn profile_metadata_can_disable_nested_delegation() {
        let runtime = runtime(3, 10, 10);
        let mut leaf = builtin_profile("scout");
        leaf.name = "leaf".to_string();
        leaf.allow_nested_subagents = false;
        let ticket = runtime.begin_launch("root", leaf).unwrap();
        runtime.bind_child(ticket.run_id(), "child").unwrap();

        assert_eq!(
            runtime
                .begin_launch("child", builtin_profile("scout"))
                .unwrap_err(),
            LaunchError::NestedDelegationDisabled {
                profile: "leaf".to_string()
            }
        );
    }

    #[test]
    fn profile_max_depth_tightens_the_inherited_limit() {
        let runtime = runtime(6, 10, 10);
        let mut limited = builtin_profile("delegate");
        limited.max_subagent_depth = Some(2);
        let first = runtime.begin_launch("root", limited).unwrap();
        let assignment = runtime.bind_child(first.run_id(), "child").unwrap();
        assert_eq!(assignment.max_depth, 2);

        let second = runtime
            .begin_launch("child", builtin_profile("delegate"))
            .unwrap();
        runtime.bind_child(second.run_id(), "grandchild").unwrap();
        assert_eq!(
            runtime
                .begin_launch("grandchild", builtin_profile("delegate"))
                .unwrap_err(),
            LaunchError::Depth {
                depth: 2,
                maximum: 2,
            }
        );
    }

    #[test]
    fn profile_cannot_relax_a_stricter_root_limit() {
        let runtime = runtime(6, 10, 10);
        let mut permissive = builtin_profile("delegate");
        permissive.max_subagent_depth = Some(6);
        let first = runtime
            .begin_launch_with_max_depth("root", permissive, 1)
            .unwrap();
        let assignment = runtime.bind_child(first.run_id(), "child").unwrap();
        assert_eq!(assignment.max_depth, 1);
        assert_eq!(
            runtime
                .begin_launch_with_max_depth("child", builtin_profile("delegate"), 6)
                .unwrap_err(),
            LaunchError::Depth {
                depth: 1,
                maximum: 1,
            }
        );
    }

    #[test]
    fn root_configuration_is_re_resolved_without_widening_existing_children() {
        let runtime = runtime(6, 10, 10);
        let first = runtime
            .begin_launch_with_max_depth("root", builtin_profile("delegate"), 1)
            .unwrap();
        runtime.bind_child(first.run_id(), "child").unwrap();

        assert!(
            runtime
                .begin_launch_with_max_depth("root", builtin_profile("delegate"), 2)
                .is_ok()
        );
        assert_eq!(
            runtime
                .begin_launch_with_max_depth("child", builtin_profile("delegate"), 2)
                .unwrap_err(),
            LaunchError::Depth {
                depth: 1,
                maximum: 1,
            }
        );
    }
}
