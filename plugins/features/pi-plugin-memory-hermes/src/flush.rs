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
    let prompt = format!(
        "The conversation is about to lose context. Preserve its durable learning now.\n\n{}",
        crate::transport::review_prompt(true, true)
    );
    let errors = match crate::transport::run_review(
        &context.session,
        &context.models,
        config,
        runs,
        &prompt,
        parent_signal.unwrap_or_else(|| AbortHandle::new().1),
        timeout,
    )
    .await
    {
        Ok(outcome) => {
            crate::transport::finish_review(&context.session, &context.ui, config, &outcome)
        }
        Err(error) => vec![error],
    };
    for error in errors {
        let _ = context.ui.notify(
            pi_core::NoticeLevel::Warning,
            format!("⚠️ Memory review: {error}"),
        );
    }
}
