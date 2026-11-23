use super::AgentSession;
use super::activity::{ActivityPhase, CancellationStage, SessionActivity};
use super::compaction::{CompactionCompleter, serialize_conversation};
use crate::{
    AgentMessage, BranchSummaryEntry, CompactionPreparation, FileOperations,
    SessionBeforeTreeEvent, SessionContext, SessionDocument, SessionError, SessionTreeEvent,
    TreePreparation, estimate_tokens, now_ms,
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
            let entry = self.store.set_label_with_entry(target_id, Some(label))?;
            self.events
                .publish_entry(entry, crate::EntryAppendOrigin::Configuration);
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
        let _operation = self.activity.begin(ActivityPhase::Summarizing).await;
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
                from_hook: None,
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
        let branch_input = prepare_branch_summary_messages(
            &preparation.entries_to_summarize,
            self.context_updates.options(),
            self.active_context_window().unwrap_or(128_000),
            self.branch_summary_reserve_tokens,
        );
        let messages = branch_input.messages;
        let (summary, usage, details) = if messages.is_empty() {
            ("No content to summarize".to_string(), None, None)
        } else {
            let mut summary_preparation = CompactionPreparation {
                first_kept_entry_id: None,
                messages_to_summarize: branch_input.inspected_messages,
                turn_prefix_messages: Vec::new(),
                retained_tail: Vec::new(),
                is_split_turn: false,
                tokens_before: 0,
                previous_summary: None,
                file_ops: FileOperations::default(),
                settings: self.compaction_settings(),
            };
            self.compaction_policy
                .prepare_branch(&preparation.entries_to_summarize, &mut summary_preparation);
            // File tracking includes the first over-budget message, but the LLM
            // receives only the messages admitted to the branch history budget.
            summary_preparation.messages_to_summarize = messages;
            let conversation = serialize_conversation(&summary_preparation.messages_to_summarize);
            let prompt = self.compaction_policy.branch_prompt();
            let instructions = match (
                preparation
                    .custom_instructions
                    .as_deref()
                    .filter(|value| !value.is_empty()),
                preparation.replace_instructions,
            ) {
                (Some(custom), true) => custom.to_string(),
                (Some(custom), false) => format!("{prompt}\n\nAdditional focus: {custom}"),
                (None, _) => prompt.to_string(),
            };
            let response = self
                .complete_summary(
                    RuntimeCompletionRequest {
                        system_prompt: self.compaction_policy.system_prompt().to_string(),
                        messages: vec![Message::User(UserMessage::text(
                            format!(
                                "<conversation>\n{conversation}\n</conversation>\n\n{instructions}"
                            ),
                            now_ms(),
                        ))],
                        model: None,
                        thinking_level: if self.runtime.model_capabilities().reasoning {
                            self.runtime.agent().state().thinking_level
                        } else {
                            pi_core::ThinkingLevel::Off
                        },
                        thinking_budgets: self.runtime.agent().thinking_budgets(),
                        max_output_tokens: None,
                        cache_retention: None,
                        routing_session_id: None,
                    },
                    signal.clone(),
                    crate::SummarizationSource::BranchSummary,
                )
                .await?;
            if signal.is_aborted() || response.stop_reason == StopReason::Aborted {
                return Err(SessionError::Cancelled("session tree navigation"));
            }
            if let Some(error) =
                super::compaction::summarization_failure(&response, "Branch summarization")
            {
                return Err(SessionError::Runtime(error));
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
            let mut summary = format!("{PREAMBLE}{text}");
            let details = self
                .compaction_policy
                .finalize(&summary_preparation, &mut summary);
            (summary, Some(response.usage), details)
        };
        Ok(BranchSummaryEntry {
            from_hook: None,
            from_id: String::new(),
            summary,
            details,
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
        summary.from_hook = Some(from_extension);
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

struct BranchSummaryMessages {
    messages: Vec<AgentMessage>,
    inspected_messages: Vec<AgentMessage>,
}

fn prepare_branch_summary_messages(
    entries: &[crate::SessionRecord],
    options: &crate::SessionContextBuildOptions,
    context_window: u64,
    reserve_tokens: u64,
) -> BranchSummaryMessages {
    let edits = entries
        .iter()
        .filter_map(|record| match &record.entry {
            crate::SessionEntry::ContextEdit(edit) => Some((edit.target_id.as_str(), edit)),
            _ => None,
        })
        .collect::<std::collections::HashMap<_, _>>();
    let candidates = entries
        .iter()
        .enumerate()
        .flat_map(|(index, record)| {
            let (messages, important) = match &record.entry {
                crate::SessionEntry::Compaction(compaction) => (
                    vec![
                AgentMessage::custom(serde_json::json!({
                    "role":"compactionSummary", "summary":compaction.summary,
                    "tokensBefore":compaction.tokens_before, "timestamp":record.timestamp_ms,
                })).expect("valid compaction summary"),
            ],
                    true,
                ),
                crate::SessionEntry::BranchSummary(branch) => (
                    vec![
                        AgentMessage::custom(serde_json::json!({
                            "role":"branchSummary", "summary":branch.summary,
                            "fromId":branch.from_id, "timestamp":record.timestamp_ms,
                        }))
                        .expect("valid branch summary"),
                    ],
                    true,
                ),
                crate::SessionEntry::Message(message) if message.message.role() != "toolResult" => {
                    (
                        edits
                            .get(record.id.as_str())
                            .map_or_else(
                                || Some(message.message.clone()),
                                |edit| edit.apply(&message.message),
                            )
                            .into_iter()
                            .collect(),
                        false,
                    )
                }
                crate::SessionEntry::CustomMessage(_) | crate::SessionEntry::Custom(_) => {
                    // Preserve Rust's injected projectors/isolated seeds without
                    // applying the active-context latest-compaction boundary.
                    let messages =
                        crate::session_entry_to_context_messages(record, index, entries, options)
                            .into_iter()
                            .filter(|message| message.role() != "toolResult")
                            .filter_map(|message| {
                                edits.get(record.id.as_str()).map_or_else(
                                    || Some(message.clone()),
                                    |edit| edit.apply(&message),
                                )
                            })
                            .collect();
                    (messages, false)
                }
                _ => (Vec::new(), false),
            };
            messages
                .into_iter()
                .map(move |message| (message, important))
        })
        .collect::<Vec<_>>();
    select_branch_summary_messages(candidates, context_window.saturating_sub(reserve_tokens))
}

fn select_branch_summary_messages(
    candidates: Vec<(AgentMessage, bool)>,
    token_budget: u64,
) -> BranchSummaryMessages {
    let mut total_tokens = 0_u64;
    let mut messages = Vec::new();
    let mut inspected_messages = Vec::new();
    for (message, important) in candidates.into_iter().rev() {
        inspected_messages.push(message.clone());
        let tokens = estimate_tokens(&message);
        if token_budget > 0 && total_tokens.saturating_add(tokens) > token_budget {
            // An existing summary is valuable context: allow it to exceed the
            // remaining budget only when less than 90% was already consumed.
            if important && u128::from(total_tokens) * 10 < u128::from(token_budget) * 9 {
                messages.push(message);
            }
            break;
        }
        total_tokens = total_tokens.saturating_add(tokens);
        messages.push(message);
    }
    messages.reverse();
    inspected_messages.reverse();
    BranchSummaryMessages {
        messages,
        inspected_messages,
    }
}

#[cfg(test)]
pub(super) fn branch_summary_input_messages(
    messages: Vec<AgentMessage>,
    context_window: u64,
    reserve_tokens: u64,
) -> Vec<AgentMessage> {
    select_branch_summary_messages(
        messages
            .into_iter()
            .filter(|message| message.role() != "toolResult")
            .map(|message| (message, false))
            .collect(),
        context_window.saturating_sub(reserve_tokens),
    )
    .messages
}

#[cfg(test)]
#[test]
fn current_oracle_branch_preparation_uses_raw_history_and_text_budget() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/compaction-current-oracle.json"
    ))
    .unwrap();
    for case in fixture["branchCases"].as_array().unwrap() {
        let entries =
            serde_json::from_value::<Vec<crate::SessionRecord>>(case["rustEntries"].clone())
                .unwrap();
        let input = prepare_branch_summary_messages(
            &entries,
            &Default::default(),
            case["budget"].as_u64().unwrap(),
            0,
        );
        assert_eq!(
            serde_json::to_value(&input.messages).unwrap(),
            case["expected"]["messages"],
            "{}",
            case["name"]
        );
        assert_eq!(
            input.messages.iter().map(estimate_tokens).sum::<u64>(),
            case["expected"]["totalTokens"].as_u64().unwrap()
        );
        if case["name"] == "branch-over-budget-file-tracking" {
            assert_eq!(input.messages.len(), 1);
            assert_eq!(input.inspected_messages.len(), 2);
        }
    }
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
