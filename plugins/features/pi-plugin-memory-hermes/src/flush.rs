//! Bounded pre-compaction/session-close preservation through the same Agent fork.
use crate::{config::HermesMemoryConfig, execution::HermesRuns};
use pi_core::{AbortHandle, AbortSignal, SessionSnapshot};
use pi_session::SessionPluginContext;
use std::{sync::Arc, time::Duration};

pub(crate) async fn flush_if_due(
    context: &SessionPluginContext,
    _snapshot: &SessionSnapshot,
    runs: Arc<HermesRuns>,
    config: &HermesMemoryConfig,
    user_turns: u64,
    parent_signal: Option<AbortSignal>,
    timeout: Duration,
) {
    if user_turns < config.flush_min_turns {
        return;
    }
    let _ = crate::transport::run_review(&context.session, &context.models, config, runs,
        "The conversation is about to lose context. Save durable user preferences and useful verified procedures using memory and skill_manage. Read existing skills before changing them. Do not save temporary task progress.",
        parent_signal.unwrap_or_else(|| AbortHandle::new().1), timeout).await;
}
