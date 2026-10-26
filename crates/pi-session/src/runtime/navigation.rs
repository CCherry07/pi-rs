use super::AgentSession;
use super::activity::ActivityPhase;
use crate::{
    AgentMessage, BranchSummaryEntry, SessionBeforeTreeEvent, SessionContext, SessionDocument,
    SessionError, SessionTreeEvent, TreePreparation, estimate_context_tokens, now_ms,
};
use pi_core::{AbortHandle, ContentBlock, Message, StopReason, UserMessage};
use pi_runtime::RuntimeCompletionRequest;

impl AgentSession {
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
        let preparation = tree_preparation(&document, leaf_id, false)?;
        let (_, signal) = AbortHandle::new();
        let before = self
            .plugin_driver()
            .session_before_tree(
                self.session_dispatch_context(),
                &SessionBeforeTreeEvent {
                    preparation,
                    signal,
                },
            )
            .await;
        if before.as_ref().is_some_and(|result| result.cancel) {
            return Err(SessionError::Cancelled("session tree navigation"));
        }
        self.context_updates.checkout(leaf_id, &context)?;
        if let Some(label) = before.and_then(|result| result.label)
            && let Some(target_id) = leaf_id
        {
            self.store.set_label(target_id, Some(label))?;
        }
        self.plugin_driver()
            .session_tree(
                self.session_dispatch_context(),
                &SessionTreeEvent {
                    new_leaf_id: self.store.leaf_id(),
                    old_leaf_id: previous_leaf,
                    summary_entry: None,
                    from_extension: None,
                },
            )
            .await;
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
        let preparation = tree_preparation(&document, leaf_id, true)?;
        let (_, signal) = AbortHandle::new();
        let before = self
            .plugin_driver()
            .session_before_tree(
                self.session_dispatch_context(),
                &SessionBeforeTreeEvent {
                    preparation,
                    signal,
                },
            )
            .await;
        if before.as_ref().is_some_and(|result| result.cancel) {
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
        summary.from_id = previous_leaf.clone().unwrap_or_else(|| "root".to_string());
        let record = self
            .context_updates
            .branch(leaf_id, summary.clone(), label)?;
        let id = record.id.clone();
        self.plugin_driver()
            .session_tree(
                self.session_dispatch_context(),
                &SessionTreeEvent {
                    new_leaf_id: self.store.leaf_id(),
                    old_leaf_id: previous_leaf,
                    summary_entry: Some(summary),
                    from_extension: Some(from_extension),
                },
            )
            .await;
        Ok(id)
    }

    /// Generates an abandoned-branch summary with the selected
    /// provider, checks out the target, and appends the summary as the new
    /// leaf. Extension `session_before_tree` hooks may still replace it.
    pub async fn summarize_branch_and_checkout(
        &self,
        leaf_id: &str,
        custom_instructions: Option<String>,
        replace_instructions: bool,
        label: Option<String>,
    ) -> Result<String, SessionError> {
        const PREAMBLE: &str = "The user explored a different conversation branch before returning here.\nSummary of that exploration:\n\n";

        self.ensure_open()?;
        let document = self.store.shared_document()?;
        let preparation = tree_preparation(&document, Some(leaf_id), true)?;
        let context = crate::build_session_context(
            &preparation.entries_to_summarize,
            self.context_updates.options(),
        );
        let summary_messages = branch_summary_input_messages(
            context.messages,
            self.active_context_window().unwrap_or(128_000),
            self.branch_summary_reserve_tokens,
        );
        let (summary, usage) = if summary_messages.is_empty() {
            ("No content to summarize".to_string(), None)
        } else {
            let conversation = serde_json::to_string_pretty(&summary_messages)
                .map_err(|error| SessionError::InvalidPayload(error.to_string()))?;
            let prompt = self.compaction_policy.branch_prompt();
            let instructions = match (custom_instructions, replace_instructions) {
                (Some(custom), true) => custom,
                (Some(custom), false) => format!("{prompt}\n\nAdditional focus: {custom}"),
                (None, _) => prompt.to_string(),
            };
            let (_, signal) = AbortHandle::new();
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
                    signal,
                )
                .await?;
            if response.stop_reason == StopReason::Error {
                return Err(SessionError::Runtime(
                    response
                        .error_message
                        .unwrap_or_else(|| "branch summarization failed".to_string()),
                ));
            }
            if response
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolCall(_)))
            {
                return Err(SessionError::Runtime(
                    "branch summarization attempted to call a tool".to_string(),
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
        let id = self
            .branch_with_summary(
                Some(leaf_id),
                BranchSummaryEntry {
                    from_id: String::new(),
                    summary,
                    details: None,
                    usage,
                },
            )
            .await?;
        if let Some(label) = label {
            self.set_label(&id, Some(label))?;
        }
        Ok(id)
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
