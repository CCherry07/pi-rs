//! Explicit user-requested consolidation. Overflow never spawns another Agent.
use crate::{
    config::HermesMemoryConfig,
    execution::HermesRuns,
    store::{HermesMemoryStore, MemoryResult, MemoryTarget},
};
use pi_core::{AbortHandle, CommandContext, EphemeralSessionStatus};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub(crate) struct ConsolidationResult {
    pub consolidated: bool,
    pub error: Option<String>,
}

/// Hermes tools/memory_tool.py: three consecutive failures may ask for another
/// attempt; the fourth only tells the Agent to finish its user-facing reply.
/// Each Hermes plugin invocation owns this value, so foreground, child and detached invocations
/// cannot reset or consume one another's budget.
#[derive(Default)]
pub(crate) struct ConsolidationBudget(Mutex<u32>);

impl ConsolidationBudget {
    pub(crate) fn observe(&self, result: MemoryResult) -> MemoryResult {
        let mut failures = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if result.success {
            *failures = 0;
        } else if result.consolidation_failure {
            *failures = failures.saturating_add(1);
            if *failures > 3 {
                return MemoryResult {
                    success: false,
                    done: Some(true),
                    error: Some(format!(
                        "Memory consolidation failed {failures} times this turn. Stop retrying memory calls — leave memory unchanged for now and continue with your reply to the user. The fact can be saved in a later turn."
                    )),
                    ..MemoryResult::default()
                };
            }
        }
        result
    }
}

pub(crate) async fn with_command_context(
    context: &CommandContext,
    store: Arc<HermesMemoryStore>,
    runs: Arc<HermesRuns>,
    target: MemoryTarget,
) -> ConsolidationResult {
    let execute = async {
        if !matches!(target, MemoryTarget::Memory | MemoryTarget::User) {
            return Err("Consolidation supports only memory and user targets.".into());
        }
        let before = store.entries(target).map_err(|e| e.to_string())?.join("\n§\n");
        let config: &HermesMemoryConfig = store.config();
        let prompt = format!("The user requested consolidation of target '{}'. Use memory to merge overlapping entries and remove stale notes. Preserve useful facts. Current entries:\n{before}", target.as_str());
        let mut request = crate::transport::request(config, runs, context.session.active_tools().map_err(|e| e.to_string())?, &prompt,
            context.models.all().map_err(|e| e.to_string())?, context.models.current().map_err(|e| e.to_string())?, Duration::from_millis(config.consolidation_timeout_ms))?;
        request.origin = "memory_consolidation".into();
        request.tools.retain(|name| name == "memory");
        let outcome = context.session.run_ephemeral(request, AbortHandle::new().1).await.map_err(|e| e.to_string())?;
        if outcome.status != EphemeralSessionStatus::Completed { return Err(format!("Consolidation stopped: {:?}", outcome.status)); }
        let after = store.entries(target).map_err(|e| e.to_string())?.join("\n§\n");
        if after.chars().count() >= before.chars().count() { return Err("No verified reduction in stored memory.".into()); }
        Ok(())
    }.await;
    ConsolidationResult {
        consolidated: execute.is_ok(),
        error: execute.err(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure() -> MemoryResult {
        MemoryResult::consolidation_error("No matching entry")
    }

    #[test]
    fn successful_write_resets_consecutive_failures_but_validation_does_not() {
        let budget = ConsolidationBudget::default();
        for _ in 0..3 {
            assert_eq!(budget.observe(failure()).done, None);
        }
        let invalid = budget.observe(MemoryResult {
            error: Some("Invalid content".into()),
            ..MemoryResult::default()
        });
        assert_eq!(invalid.done, None);
        assert_eq!(budget.observe(failure()).done, Some(true));
        let success = budget.observe(MemoryResult {
            success: true,
            ..MemoryResult::default()
        });
        assert!(success.success);
        assert_eq!(budget.observe(failure()).done, None);
    }
}
