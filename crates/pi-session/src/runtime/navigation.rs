use super::AgentSession;
use super::activity::{ActivityPhase, CancellationStage, SessionActivity};
use crate::{
    AgentMessage, BranchSummaryEntry, SessionBeforeTreeEvent, SessionContext, SessionDocument,
    SessionError, SessionTreeEvent, TreePreparation, estimate_context_tokens, now_ms,
};
use pi_core::{AbortHandle, AbortSignal, ContentBlock, Message, StopReason, UserMessage};
use pi_runtime::RuntimeCompletionRequest;

/// The operation gate owns this slot; every exit clears it, including errors.
struct TreeCancellation<'a>(&'a SessionActivity);
impl Drop for TreeCancellation<'_> {
    fn drop(&mut self) {
        self.0.clear_cancellation(CancellationStage::Boundary);
    }
}

impl AgentSession {
    fn tree_cancellation(&self) -> (TreeCancellation<'_>, AbortSignal) {
        let (handle, signal) = AbortHandle::new();
        self.activity
            .register_cancellation(CancellationStage::Boundary, handle);
        (TreeCancellation(&self.activity), signal)
    }

    pub async fn checkout(&self, leaf_id: Option<&str>) -> Result<SessionContext, SessionError> {
        let _operation = self.activity.begin(ActivityPhase::Updating).await;
        self.ensure_open()?;
        let document = self.store.shared_document()?;
        let context = self
            .store
            .context_at_with_options(leaf_id, self.context_updates.options())?;
        let previous_leaf = self.store.leaf_id();
        if previous_leaf.as_deref() == leaf_id {
            return Ok(context);
        }
        let (_cancellation, signal) = self.tree_cancellation();
        let before = self
            .plugin_driver()
            .session_before_tree(
                self.session_dispatch_context(),
                &SessionBeforeTreeEvent {
                    preparation: tree_preparation(&document, leaf_id, false)?,
                    signal: signal.clone(),
                },
            )
            .await;
        if signal.is_aborted() || before.as_ref().is_some_and(|result| result.cancel) {
            return Err(SessionError::Cancelled("session tree navigation"));
        }
        self.context_updates.checkout(leaf_id, &context)?;
        if let Some(label) = before.and_then(|result| result.label)
            && let Some(target_id) = leaf_id
        {
            self.store.set_label(target_id, Some(label))?;
        }
        self.finish_tree_change(previous_leaf, None, None).await?;
        Ok(context)
    }

    pub async fn branch_with_summary(
        &self,
        leaf_id: Option<&str>,
        mut summary: BranchSummaryEntry,
    ) -> Result<String, SessionError> {
        let _operation = self.activity.begin(ActivityPhase::Updating).await;
        self.ensure_open()?;
        let document = self.store.shared_document()?;
        self.store
            .context_at_with_options(leaf_id, self.context_updates.options())?;
        let previous_leaf = self.store.leaf_id();
        let (_cancellation, signal) = self.tree_cancellation();
        let before = self
            .plugin_driver()
            .session_before_tree(
                self.session_dispatch_context(),
                &SessionBeforeTreeEvent {
                    preparation: tree_preparation(&document, leaf_id, true)?,
                    signal: signal.clone(),
                },
            )
            .await;
        if signal.is_aborted() || before.as_ref().is_some_and(|result| result.cancel) {
            return Err(SessionError::Cancelled("session tree navigation"));
        }
        let mut from_extension = false;
        let mut label = None;
        if let Some(result) = before {
            if let Some(replacement) = result.summary {
                summary.summary = replacement.summary;
                summary.details = replacement.details;
                summary.usage = replacement.usage;
                from_extension = true;
            }
            label = result.label;
        }
        self.commit_summary_branch(leaf_id, previous_leaf, summary, label, from_extension)
            .await
    }

    /// Generates a tool-free abandoned-branch summary under the same operation
    /// gate as checkout. Hooks run before the provider and may replace or cancel it.
    pub async fn summarize_branch_and_checkout(
        &self,
        leaf_id: &str,
        custom_instructions: Option<String>,
        replace_instructions: bool,
        label: Option<String>,
    ) -> Result<String, SessionError> {
        let _operation = self.activity.begin(ActivityPhase::Updating).await;
        self.ensure_open()?;
        let document = self.store.shared_document()?;
        self.store
            .context_at_with_options(Some(leaf_id), self.context_updates.options())?;
        let previous_leaf = self.store.leaf_id();
        let mut preparation = tree_preparation(&document, Some(leaf_id), true)?;
        preparation.custom_instructions = custom_instructions;
        preparation.replace_instructions = replace_instructions;
        preparation.label = label;
        let (_cancellation, signal) = self.tree_cancellation();
        let before = self
            .plugin_driver()
            .session_before_tree(
                self.session_dispatch_context(),
                &SessionBeforeTreeEvent {
                    preparation: preparation.clone(),
                    signal: signal.clone(),
                },
            )
            .await;
        if signal.is_aborted() || before.as_ref().is_some_and(|result| result.cancel) {
            return Err(SessionError::Cancelled("session tree navigation"));
        }
        let before = before.unwrap_or_default();
        let from_extension = before.summary.is_some();
        if let Some(value) = before.custom_instructions {
            preparation.custom_instructions = Some(value);
        }
        if let Some(value) = before.replace_instructions {
            preparation.replace_instructions = value;
        }
        if let Some(value) = before.label {
            preparation.label = Some(value);
        }
        let summary = if let Some(summary) = before.summary {
            BranchSummaryEntry {
                from_id: String::new(),
                summary: summary.summary,
                details: summary.details,
                usage: summary.usage,
            }
        } else {
            self.generate_branch_summary(&preparation, signal.clone())
                .await?
        };
        if signal.is_aborted() {
            return Err(SessionError::Cancelled("session tree navigation"));
        }
        self.commit_summary_branch(
            Some(leaf_id),
            previous_leaf,
            summary,
            preparation.label,
            from_extension,
        )
        .await
    }

    async fn generate_branch_summary(
        &self,
        preparation: &TreePreparation,
        signal: AbortSignal,
    ) -> Result<BranchSummaryEntry, SessionError> {
        const PREAMBLE: &str = "The user explored a different conversation branch before returning here.\nSummary of that exploration:\n\n";
        let context = crate::build_session_context(
            &preparation.entries_to_summarize,
            self.context_updates.options(),
        );
        let messages = branch_summary_input_messages(
            context.messages,
            self.active_context_window().unwrap_or(128_000),
            self.branch_summary_reserve_tokens,
        );
        let (summary, usage) = if messages.is_empty() {
            ("No content to summarize".to_string(), None)
        } else {
            let conversation = serde_json::to_string_pretty(&messages)
                .map_err(|error| SessionError::InvalidPayload(error.to_string()))?;
            let prompt = self.compaction_policy.branch_prompt();
            let instructions = match (
                &preparation.custom_instructions,
                preparation.replace_instructions,
            ) {
                (Some(custom), true) => custom.clone(),
                (Some(custom), false) => format!("{prompt}\n\nAdditional focus: {custom}"),
                (None, _) => prompt.to_string(),
            };
            let response = self
                .runtime
                .complete(
                    RuntimeCompletionRequest {
                        system_prompt: self.compaction_policy.system_prompt().to_string(),
                        messages: vec![Message::User(UserMessage::text(
                            format!(
                                "<conversation>\n{conversation}\n</conversation>\n\n{instructions}"
                            ),
                            now_ms(),
                        ))],
                        model: None,
                        thinking_level: self.runtime.agent().state().thinking_level,
                        thinking_budgets: self.runtime.agent().thinking_budgets(),
                        max_output_tokens: Some(2_048),
                    },
                    signal.clone(),
                )
                .await?;
            if signal.is_aborted() || response.stop_reason == StopReason::Aborted {
                return Err(SessionError::Cancelled("session tree navigation"));
            }
            if response.stop_reason == StopReason::Error {
                return Err(SessionError::Runtime(
                    response
                        .error_message
                        .unwrap_or_else(|| "branch summarization failed".into()),
                ));
            }
            if response
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolCall(_)))
            {
                return Err(SessionError::Runtime(
                    "branch summarization attempted to call a tool".into(),
                ));
            }
            let text = response
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            (
                format!(
                    "{PREAMBLE}{}",
                    if text.is_empty() {
                        "No summary generated"
                    } else {
                        &text
                    }
                ),
                Some(response.usage),
            )
        };
        Ok(BranchSummaryEntry {
            from_id: String::new(),
            summary,
            details: None,
            usage,
        })
    }

    async fn commit_summary_branch(
        &self,
        leaf_id: Option<&str>,
        previous_leaf: Option<String>,
        mut summary: BranchSummaryEntry,
        label: Option<String>,
        from_extension: bool,
    ) -> Result<String, SessionError> {
        summary.from_id = previous_leaf.clone().unwrap_or_else(|| "root".to_string());
        let record = self
            .context_updates
            .branch(leaf_id, summary.clone(), label)?;
        self.finish_tree_change(previous_leaf, Some(summary), Some(from_extension))
            .await?;
        Ok(record.id)
    }

    async fn finish_tree_change(
        &self,
        old_leaf_id: Option<String>,
        summary_entry: Option<BranchSummaryEntry>,
        from_extension: Option<bool>,
    ) -> Result<(), SessionError> {
        // Publish immutable native history on the ordered event stream. Command
        // replies must never drive transcript replacement or revision cursors.
        self.events
            .publish_tree(self.store.shared_document()?, self.runtime.agent().state());
        self.plugin_driver()
            .session_tree(
                self.session_dispatch_context(),
                &SessionTreeEvent {
                    new_leaf_id: self.store.leaf_id(),
                    old_leaf_id,
                    summary_entry,
                    from_extension,
                },
            )
            .await;
        Ok(())
    }
}

pub(super) fn branch_summary_input_messages(
    messages: Vec<AgentMessage>,
    context_window: u64,
    reserve_tokens: u64,
) -> Vec<AgentMessage> {
    let messages = messages
        .into_iter()
        .filter(|message| !matches!(message.as_standard(), Some(Message::ToolResult(_))))
        .collect::<Vec<_>>();
    let Some(token_budget) = context_window.checked_sub(reserve_tokens) else {
        return messages;
    };
    if token_budget == 0 {
        return messages;
    }
    let mut total_tokens = 0_u64;
    let mut selected = Vec::new();
    for message in messages.into_iter().rev() {
        let tokens = estimate_context_tokens(std::slice::from_ref(&message)).tokens;
        if total_tokens.saturating_add(tokens) > token_budget {
            break;
        }
        total_tokens = total_tokens.saturating_add(tokens);
        selected.push(message);
    }
    selected.reverse();
    selected
}

pub(super) fn tree_preparation(
    document: &SessionDocument,
    target_id: Option<&str>,
    user_wants_summary: bool,
) -> Result<TreePreparation, SessionError> {
    let old_branch = document.branch()?;
    let target_branch = document.branch_at(target_id)?;
    let common_len = old_branch
        .iter()
        .zip(&target_branch)
        .take_while(|(old, target)| old.id == target.id)
        .count();
    let common_ancestor_id = common_len
        .checked_sub(1)
        .map(|index| old_branch[index].id.clone());
    Ok(TreePreparation {
        target_id: target_id.map(str::to_string),
        old_leaf_id: document.leaf_id(crate::MAIN_LANE)?.map(str::to_string),
        common_ancestor_id,
        entries_to_summarize: old_branch[common_len..]
            .iter()
            .map(|entry| (*entry).clone())
            .collect(),
        user_wants_summary,
        custom_instructions: None,
        replace_instructions: false,
        label: None,
    })
}
