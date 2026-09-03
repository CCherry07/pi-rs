//! Scoped, in-memory sessions. Reuses Agent's tool loop, not a second engine.

use std::sync::Arc;

use pi_agent::{
    Agent, AgentLoopStop, AgentLoopTurnUpdate, AgentOptions, AgentRuntime, FnTurnControl,
};
use pi_core::{
    AbortSignal, EphemeralSessionOutcome, EphemeralSessionRequest, EphemeralSessionStatus, Message,
    ModelSelection, PluginDriver,
};

use crate::{PiRuntime, RuntimeError};

mod compaction;

struct AbortOnDrop(Agent);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests;

impl PiRuntime {
    /// Runs one bounded, in-memory Agent without creating a managed session or
    /// persisting its transcript. Scheduling it in the background is up to the
    /// caller; completed tool side effects are not rolled back on exit.
    ///
    /// See [`EphemeralSessionRequest::plugins`] for the private plugin hooks,
    /// unavailable lifecycle events/capabilities, and cancellation cleanup rules.
    pub async fn run_ephemeral(
        &self,
        request: EphemeralSessionRequest,
        signal: AbortSignal,
    ) -> Result<EphemeralSessionOutcome, RuntimeError> {
        if request.timeout.is_zero() || request.max_tool_iterations == 0 {
            return Err(RuntimeError::Build(
                "ephemeral sessions require positive timeout and tool-iteration limits".to_string(),
            ));
        }
        if request.compaction.as_ref().is_some_and(|options| {
            options.threshold_tokens == 0
                || options.retained_tail_tokens == 0
                || options.max_summary_tokens == 0
        }) {
            return Err(RuntimeError::Build(
                "ephemeral compaction requires positive token limits".into(),
            ));
        }
        if signal.is_aborted() {
            return Ok(EphemeralSessionOutcome {
                messages: Vec::new(),
                status: EphemeralSessionStatus::Aborted,
            });
        }
        // Explicit private attachments use the ordinary Agent hook driver.
        // Parent hooks are not inherited, and register() is not invoked:
        // registrations stay in the immutable parent generation.
        let plugins = PluginDriver::new(request.plugins)
            .map_err(|error| RuntimeError::Build(error.to_string()))?;
        // No reload or session-manager mutex: callers may already hold one in
        // an awaited lifecycle hook. This lease keeps auth adapters alive.
        let generation = self.current_generation();
        let parent = self.agent.state();
        for name in &request.tools {
            if !parent.active_tools.contains(name) {
                return Err(RuntimeError::UnknownTools(format!(
                    "{name} is outside the calling session's active tools"
                )));
            }
        }
        let model = request.model.unwrap_or(ModelSelection {
            provider: parent.provider_id,
            model_id: parent.model_id,
        });
        if generation
            .agent
            .registries()
            .provider(&model.provider)
            .is_none()
        {
            return Err(RuntimeError::Provider(format!(
                "provider not found: {}",
                model.provider
            )));
        }
        let registries = generation
            .agent
            .registries()
            .scope_tool_execution(&request.tools, &request.origin);
        // No parent hooks or tool-context capabilities. Tools cannot
        // launch children, notify the UI, or mutate the parent session.
        let input_limit = request.max_input_tokens;
        let summary_input = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let compaction_error = Arc::new(std::sync::Mutex::new(None::<String>));
        let compactor = request.compaction.map(|options| {
            Arc::new(compaction::DetachedCompactor::new(
                Arc::clone(&generation.agent),
                model.clone(),
                self.cwd().to_path_buf(),
                self.agent.session_id(),
                options,
            ))
        });
        let exhausted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let budget_exhausted = Arc::clone(&exhausted);
        let iterations_exhausted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let iteration_flag = Arc::clone(&iterations_exhausted);
        let iteration_limit = request.max_tool_iterations;
        let summary_spent = Arc::clone(&summary_input);
        let compact_failure = Arc::clone(&compaction_error);
        let control = FnTurnControl::new().with_prepare_next_turn(move |turn, signal| {
            let compactor = compactor.clone();
            let summary_spent = Arc::clone(&summary_spent);
            let compact_failure = Arc::clone(&compact_failure);
            async move {
                let spent = spent_input(&turn.new_messages)
                    .saturating_add(summary_spent.load(std::sync::atomic::Ordering::Acquire));
                // This callback is only reached after a provider response and
                // its balanced tool results. Never compact the first replay,
                // a final answer, or a run that has already exhausted a bound.
                if turn.message.tool_calls().is_empty()
                    || model_iterations(&turn.new_messages) >= iteration_limit
                    || input_limit.is_some_and(|limit| spent >= limit)
                {
                    return Ok(None);
                }
                let Some(compactor) = compactor else {
                    return Ok(None);
                };
                match compactor
                    .compact(
                        &turn,
                        input_limit.map(|limit| limit.saturating_sub(spent)),
                        signal,
                    )
                    .await
                {
                    Ok(compacted) => {
                        summary_spent
                            .fetch_add(compacted.input_tokens, std::sync::atomic::Ordering::AcqRel);
                        Ok(compacted.context.map(|context| AgentLoopTurnUpdate {
                            context: Some(context),
                            ..AgentLoopTurnUpdate::default()
                        }))
                    }
                    Err(error) => {
                        *compact_failure
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
                        Ok(None)
                    }
                }
            }
        });
        let summary_spent = Arc::clone(&summary_input);
        let compact_failure = Arc::clone(&compaction_error);
        let control = control.with_should_stop_after_turn(move |turn, _| {
            let budget_exhausted = Arc::clone(&budget_exhausted);
            let iteration_flag = Arc::clone(&iteration_flag);
            let summary_spent = Arc::clone(&summary_spent);
            let compact_failure = Arc::clone(&compact_failure);
            async move {
                let spent = spent_input(&turn.new_messages)
                    .saturating_add(summary_spent.load(std::sync::atomic::Ordering::Acquire));
                let stop = input_limit.is_some_and(|limit| spent >= limit)
                    && !turn.message.tool_calls().is_empty();
                budget_exhausted.store(stop, std::sync::atomic::Ordering::Release);
                let iterations = model_iterations(&turn.new_messages);
                let iteration_stop =
                    iterations >= iteration_limit && !turn.message.tool_calls().is_empty();
                iteration_flag.store(iteration_stop, std::sync::atomic::Ordering::Release);
                Ok(stop
                    || iteration_stop
                    || compact_failure
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .is_some())
            }
        });
        let child = Agent::with_runtime(
            AgentOptions {
                provider_id: model.provider,
                model_id: model.model_id,
                thinking_level: request.thinking_level.unwrap_or(parent.thinking_level),
                thinking_budgets: self.agent.thinking_budgets(),
                active_tools: parent.active_tools,
                messages: if request.inherit_history {
                    match request.history_tail {
                        Some(tail) => digest_history(parent.messages, tail),
                        None => parent.messages,
                    }
                } else {
                    Vec::new()
                },
                max_tool_iterations: request.max_tool_iterations,
                turn_control: Arc::new(control),
                cwd: self.cwd().to_path_buf(),
                ..AgentOptions::default()
            },
            Arc::new(AgentRuntime::new(
                generation.agent.generation(),
                request
                    .system_prompt
                    .unwrap_or_else(|| self.agent.effective_system_prompt()),
                Arc::new(registries),
                Arc::new(plugins),
                Arc::clone(generation.agent.provider_plugins()),
            )),
        );
        child.set_session_id(self.agent.session_id());
        let _abort = AbortOnDrop(child.clone());
        // Cancellation/timeout drops child.prompt instead of awaiting its
        // terminal hooks. AbortOnDrop and plugin-owned RAII handle cleanup.
        let outcome = tokio::select! {
            biased;
            () = signal.wait() => return Ok(EphemeralSessionOutcome {
                messages: Vec::new(), status: EphemeralSessionStatus::Aborted,
            }),
            () = tokio::time::sleep(request.timeout) => return Ok(EphemeralSessionOutcome {
                messages: Vec::new(), status: EphemeralSessionStatus::TimedOut,
            }),
            result = child.prompt(request.messages) => result.map_err(|error| RuntimeError::Agent(error.to_string()))?,
        };
        let status = if exhausted.load(std::sync::atomic::Ordering::Acquire) {
            EphemeralSessionStatus::Failed("aggregate input-token budget exhausted".into())
        } else if iterations_exhausted.load(std::sync::atomic::Ordering::Acquire) {
            EphemeralSessionStatus::Failed("maximum tool iterations exceeded".into())
        } else if let Some(error) = compaction_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            EphemeralSessionStatus::Failed(error)
        } else {
            match outcome.stop {
                AgentLoopStop::Completed => EphemeralSessionStatus::Completed,
                AgentLoopStop::Aborted => EphemeralSessionStatus::Aborted,
                AgentLoopStop::MaxToolIterations => {
                    EphemeralSessionStatus::Failed("maximum tool iterations exceeded".to_string())
                }
                AgentLoopStop::ProviderError => EphemeralSessionStatus::Failed(
                    outcome
                        .new_messages
                        .iter()
                        .rev()
                        .find_map(|message| match message {
                            Message::Assistant(message) => message.error_message.clone(),
                            _ => None,
                        })
                        .unwrap_or_else(|| "provider failed".to_string()),
                ),
                AgentLoopStop::TerminatedByTools => {
                    EphemeralSessionStatus::Failed("terminated by tools".to_string())
                }
            }
        };
        Ok(EphemeralSessionOutcome {
            messages: outcome.new_messages,
            status,
        })
    }
}

fn spent_input(messages: &[Message]) -> u64 {
    messages.iter().fold(0_u64, |spent, message| match message {
        Message::Assistant(message) => {
            spent.saturating_add(compaction::input_tokens(&message.usage))
        }
        _ => spent,
    })
}

fn model_iterations(messages: &[Message]) -> usize {
    messages
        .iter()
        .filter(|message| matches!(message, Message::Assistant(_)))
        .count()
}

/// Bounded cold-cache replay. Keep complete tool-call/result groups at the cut.
fn digest_history(messages: Vec<Message>, tail: usize) -> Vec<Message> {
    let mut cut = messages.len().saturating_sub(tail.max(1));
    while cut > 0 && matches!(messages.get(cut), Some(Message::ToolResult(_))) {
        cut -= 1;
    }
    if cut == 0 {
        return messages;
    }
    let mut lines = Vec::new();
    for message in &messages[..cut] {
        let (role, content, limit) = match message {
            Message::User(message) => ("USER", &message.content, 300),
            Message::Assistant(message) => {
                let calls = message.tool_calls();
                if !calls.is_empty() {
                    lines.push(format!(
                        "ASSISTANT[tools: {}]",
                        calls
                            .iter()
                            .map(|call| call.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                ("ASSISTANT", &message.content, 200)
            }
            _ => continue,
        };
        let text = content
            .iter()
            .filter_map(|block| match block {
                pi_core::ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ")
            .replace('\n', " ");
        if !text.is_empty() {
            lines.push(format!(
                "{role}: {}",
                text.chars().take(limit).collect::<String>()
            ));
        }
    }
    let digest = Message::User(pi_core::UserMessage::text(
        format!(
            "[Earlier conversation digest; recent messages follow verbatim.]\n{}",
            lines.join("\n")
        ),
        0,
    ));
    std::iter::once(digest)
        .chain(messages.into_iter().skip(cut))
        .collect()
}
