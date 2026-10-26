use super::context::{PiPluginContext, context_failed, require_command, value, values};
use crate::{
    MAIN_LANE, QueueKind, SessionDocument, SessionEntryType, SessionRecord, build_context_entries,
    current_session_context_tokens,
};
use async_trait::async_trait;
use pi_core::{AbortSignal, AssistantMessage, CustomMessageContent, CustomMessageInput, Usage};
use pi_plugin::{
    CompactOptions, DirectCompletionRequest, ForkOptions, IsolatedFollowUpReceipt,
    IsolatedMessageReceipt, IsolatedSessionId, IsolatedSessionOutcome, IsolatedSessionRequest,
    IsolatedSessionTurnId, MessageDelivery, NavigateTreeOptions, NewSessionOptions, NoticeLevel,
    PluginContextError, PluginContextReplacement, PluginContextScope, SendMessageOptions,
    SendUserMessageOptions, SessionContextAccess, SessionEntryKind, SessionEntryView,
    SessionSnapshot,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

#[async_trait]
impl SessionContextAccess for PiPluginContext {
    fn execution_origin(&self) -> Result<pi_plugin::SessionExecutionOrigin, PluginContextError> {
        Ok(self.session()?.runtime().execution_origin())
    }

    async fn run_ephemeral(
        &self,
        _scope: PluginContextScope,
        request: pi_plugin::EphemeralSessionRequest,
        signal: AbortSignal,
    ) -> Result<pi_plugin::EphemeralSessionOutcome, PluginContextError> {
        self.session()?
            .runtime()
            .run_ephemeral(request, signal)
            .await
            .map_err(context_failed)
    }

    fn session_snapshot(&self) -> Result<SessionSnapshot, PluginContextError> {
        let session = self.session()?;
        let document = session
            .store()
            .shared_document()
            .map_err(|error| PluginContextError::Failed(error.to_string()))?;
        let leaf_id = document
            .leaf_id(MAIN_LANE)
            .map_err(|error| PluginContextError::Failed(error.to_string()))?
            .map(str::to_owned);
        let branch_ids = document
            .branch()
            .map_err(|error| PluginContextError::Failed(error.to_string()))?
            .into_iter()
            .map(|record| record.id.clone())
            .collect::<Vec<_>>();
        let entries = document
            .entries
            .iter()
            .map(session_entry_view)
            .collect::<Result<Vec<_>, _>>()?;
        let entry_indices = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.id(), index))
            .collect::<HashMap<_, _>>();
        let branch = branch_ids
            .iter()
            .map(|id| {
                entry_indices
                    .get(id.as_str())
                    .map(|&index| entries[index].clone())
                    .ok_or_else(|| {
                        PluginContextError::Failed(format!(
                            "session branch entry is missing from the snapshot: {id}"
                        ))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let path = session.store().path();
        let directory = path
            .parent()
            .unwrap_or_else(|| std::path::Path::new(""))
            .to_path_buf();
        let file = session
            .store()
            .is_materialized()
            .then(|| path.to_path_buf());
        let raw_header = value(&document.header)?;

        Ok(SessionSnapshot::new(
            document.header.id.clone(),
            document.header.cwd.clone(),
            directory,
            file,
            document.name.clone(),
            leaf_id,
            entries,
            branch,
            document
                .labels
                .iter()
                .map(|(target, label)| (target.clone(), label.clone()))
                .collect::<BTreeMap<_, _>>(),
            raw_header,
        ))
    }

    fn cwd(&self) -> Result<PathBuf, PluginContextError> {
        Ok(self.session()?.runtime().cwd().to_path_buf())
    }

    fn is_project_trusted(&self) -> Result<bool, PluginContextError> {
        let session = self.session()?;
        Ok(self
            .binding
            .project_trust(session.store().id())
            .unwrap_or(self.project_trusted))
    }

    fn is_idle(&self) -> Result<bool, PluginContextError> {
        Ok(self.session()?.is_idle())
    }

    fn has_pending_messages(&self) -> Result<bool, PluginContextError> {
        Ok(self.session()?.has_pending_messages())
    }

    fn context_usage(&self) -> Result<Option<pi_plugin::ContextUsage>, PluginContextError> {
        let session = self.session()?;
        let Some(context_window) = session.active_context_window() else {
            return Ok(None);
        };
        let branch = session
            .store()
            .branch_entries()
            .map_err(|error| PluginContextError::Failed(error.to_string()))?;
        let context = session
            .store()
            .context()
            .map_err(|error| PluginContextError::Failed(error.to_string()))?;
        let tokens =
            current_session_context_tokens(&branch, &context.messages).map(|usage| usage.tokens);
        let percent = tokens.map(|tokens| {
            if context_window == 0 {
                0.0
            } else {
                tokens as f64 / context_window as f64 * 100.0
            }
        });
        Ok(Some(pi_plugin::ContextUsage {
            tokens,
            context_window,
            percent,
        }))
    }

    fn system_prompt(&self) -> Result<String, PluginContextError> {
        Ok(self
            .session()?
            .runtime()
            .agent()
            .runtime()
            .system_prompt()
            .to_owned())
    }

    fn system_prompt_options(
        &self,
        scope: PluginContextScope,
    ) -> Result<Value, PluginContextError> {
        require_command(scope, "getSystemPromptOptions")?;
        let session = self.session()?;
        let runtime = session.runtime();
        Ok(runtime
            .prompt_options()
            .unwrap_or_else(|| json!({ "cwd": runtime.cwd() })))
    }

    fn session_cwd(&self) -> Result<PathBuf, PluginContextError> {
        self.cwd()
    }

    fn session_dir(&self) -> Result<PathBuf, PluginContextError> {
        Ok(self
            .session()?
            .store()
            .path()
            .parent()
            .unwrap_or_else(|| std::path::Path::new(""))
            .to_path_buf())
    }

    fn session_id(&self) -> Result<String, PluginContextError> {
        Ok(self.session()?.store().id().to_owned())
    }

    fn session_file(&self) -> Result<Option<PathBuf>, PluginContextError> {
        let session = self.session()?;
        Ok(session
            .store()
            .is_materialized()
            .then(|| session.store().path().to_path_buf()))
    }

    fn session_leaf_id(&self) -> Result<Option<String>, PluginContextError> {
        Ok(self.session()?.store().leaf_id())
    }

    fn session_leaf_entry(&self) -> Result<Option<Value>, PluginContextError> {
        let session = self.session()?;
        session
            .store()
            .leaf_id()
            .and_then(|id| session.store().get_entry(&id))
            .as_ref()
            .map(value)
            .transpose()
    }

    fn session_entry(&self, id: &str) -> Result<Option<Value>, PluginContextError> {
        self.session()?
            .store()
            .get_entry(id)
            .as_ref()
            .map(value)
            .transpose()
    }

    fn session_label(&self, id: &str) -> Result<Option<String>, PluginContextError> {
        Ok(self.session()?.store().label(id))
    }

    fn session_branch(&self, from_id: Option<&str>) -> Result<Vec<Value>, PluginContextError> {
        let (_, document) = self.document()?;
        let branch = document
            .branch_at(from_id.or_else(|| document.leaf_id(MAIN_LANE).ok().flatten()))
            .map_err(|error| PluginContextError::Failed(error.to_string()))?;
        values(branch)
    }

    fn session_context_entries(&self) -> Result<Vec<Value>, PluginContextError> {
        let (_, document) = self.document()?;
        let branch = document
            .branch()
            .map_err(|error| PluginContextError::Failed(error.to_string()))?
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        values(build_context_entries(
            &branch,
            &crate::SessionContextBuildOptions::default(),
        ))
    }

    fn session_header(&self) -> Result<Value, PluginContextError> {
        value(self.session()?.store().header())
    }

    fn session_entries(&self) -> Result<Vec<Value>, PluginContextError> {
        let (_, document) = self.document()?;
        values(&document.entries)
    }

    fn session_tree(&self) -> Result<Vec<Value>, PluginContextError> {
        let (_, document) = self.document()?;
        values(session_tree(&document))
    }

    fn session_name(&self) -> Result<Option<String>, PluginContextError> {
        Ok(self.session()?.store().name())
    }

    fn active_tools(&self) -> Result<Vec<String>, PluginContextError> {
        Ok(self.session()?.runtime().active_tools())
    }

    fn all_tools(&self) -> Result<Vec<pi_core::ToolSpec>, PluginContextError> {
        Ok(self.session()?.runtime().tool_specs())
    }

    fn commands(&self) -> Result<Vec<pi_plugin::CommandSpec>, PluginContextError> {
        Ok(self.session()?.runtime().command_specs())
    }

    fn abort(&self) -> Result<(), PluginContextError> {
        self.session()?.abort();
        Ok(())
    }

    fn compact(&self, options: CompactOptions) -> Result<(), PluginContextError> {
        let session = self.session()?;
        self.runtime.spawn(async move {
            let _ = session.compact(options.custom_instructions).await;
        });
        Ok(())
    }

    fn shutdown(&self) -> Result<(), PluginContextError> {
        if let Ok(session) = self.pi_session() {
            session.abort();
        }
        self.binding.request_shutdown();
        Ok(())
    }

    fn send_message(
        &self,
        message: CustomMessageInput,
        options: SendMessageOptions,
    ) -> Result<(), PluginContextError> {
        self.session()?
            .dispatch_message(custom_delivery(message, options), &self.runtime)
            .map_err(delivery_failed)
    }

    fn send_user_message(
        &self,
        content: CustomMessageContent,
        options: SendUserMessageOptions,
    ) -> Result<(), PluginContextError> {
        let session = self.session()?;
        match Arc::clone(&session).dispatch_message(user_delivery(content, options), &self.runtime)
        {
            Err(crate::SessionError::InvalidPayload(message)) => {
                session.notify_plugin(message, NoticeLevel::Error);
                Ok(())
            }
            result => result.map_err(delivery_failed),
        }
    }

    fn append_entry(
        &self,
        custom_type: String,
        data: Option<Value>,
    ) -> Result<(), PluginContextError> {
        self.session()?
            .append_custom_entry_from_plugin(custom_type, data)
            .map_err(context_failed)?;
        Ok(())
    }

    fn record_usage(&self, usage: Usage, details: Option<Value>) -> Result<(), PluginContextError> {
        self.session()?
            .record_usage(usage, details)
            .map_err(context_failed)?;
        Ok(())
    }

    fn set_session_name(&self, name: String) -> Result<(), PluginContextError> {
        let session = self.session()?;
        let name = session
            .set_name_immediate(Some(name))
            .map_err(context_failed)?;
        self.runtime.spawn(async move {
            session
                .plugin_driver()
                .session_info_changed(
                    session.session_dispatch_context(),
                    &crate::SessionInfoChangedEvent { name },
                )
                .await;
        });
        Ok(())
    }

    fn set_label(&self, entry_id: String, label: Option<String>) -> Result<(), PluginContextError> {
        self.session()?
            .set_label(&entry_id, label)
            .map_err(context_failed)
    }

    fn set_active_tools(&self, tool_names: Vec<String>) -> Result<(), PluginContextError> {
        let session = self.session()?;
        let available = session
            .runtime()
            .tool_specs()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<HashSet<_>>();
        session
            .set_active_tools(
                tool_names
                    .into_iter()
                    .filter(|tool| available.contains(tool)),
            )
            .map_err(context_failed)
    }

    async fn wait_for_idle(&self, scope: PluginContextScope) -> Result<(), PluginContextError> {
        require_command(scope, "waitForIdle")?;
        self.session()?.wait_for_idle().await;
        Ok(())
    }

    async fn send_message_and_wait(
        &self,
        scope: PluginContextScope,
        message: CustomMessageInput,
        options: SendMessageOptions,
    ) -> Result<(), PluginContextError> {
        require_command(scope, "sendMessage")?;
        self.session()?
            .deliver_message(custom_delivery(message, options))
            .await
            .map_err(delivery_failed)
    }

    async fn send_user_message_and_wait(
        &self,
        scope: PluginContextScope,
        content: CustomMessageContent,
        options: SendUserMessageOptions,
    ) -> Result<(), PluginContextError> {
        require_command(scope, "sendUserMessage")?;
        self.session()?
            .deliver_message(user_delivery(content, options))
            .await
            .map_err(delivery_failed)
    }

    async fn complete(
        &self,
        _scope: PluginContextScope,
        request: DirectCompletionRequest,
        signal: AbortSignal,
    ) -> Result<AssistantMessage, PluginContextError> {
        let session = self.session()?;
        let runtime = session.runtime();
        let state = runtime.agent().state();
        runtime
            .complete(
                pi_runtime::RuntimeCompletionRequest {
                    system_prompt: request.system_prompt,
                    messages: request.messages,
                    model: request.model,
                    thinking_level: request.thinking_level.unwrap_or(state.thinking_level),
                    thinking_budgets: runtime.agent().thinking_budgets(),
                    max_output_tokens: request.max_output_tokens,
                },
                signal,
            )
            .await
            .map_err(context_failed)
    }

    fn isolated_fork_point(
        &self,
    ) -> Result<Option<pi_plugin::IsolatedForkPoint>, PluginContextError> {
        self.session()?
            .isolated_fork_point()
            .map_err(context_failed)
    }

    async fn launch_isolated_session(
        &self,
        _scope: PluginContextScope,
        request: IsolatedSessionRequest,
    ) -> Result<IsolatedSessionId, PluginContextError> {
        self.pi_session()?
            .launch_isolated_session(request)
            .await
            .map_err(context_failed)
    }

    async fn wait_for_isolated_session(
        &self,
        _scope: PluginContextScope,
        id: IsolatedSessionId,
    ) -> Result<IsolatedSessionOutcome, PluginContextError> {
        // Capture only the receipt channel before awaiting. Retaining a parent
        // PiSession here would keep its plugin runtime (and monitor owner) alive.
        let waiting = self.pi_session()?.isolated_session_waiter(&id)?;
        waiting.await
    }

    async fn wait_for_isolated_session_turn(
        &self,
        _scope: PluginContextScope,
        id: IsolatedSessionId,
        turn_id: IsolatedSessionTurnId,
    ) -> Result<IsolatedSessionOutcome, PluginContextError> {
        let waiting = self
            .pi_session()?
            .isolated_session_turn_waiter(&id, &turn_id)?;
        waiting.await
    }

    fn send_to_isolated_session(
        &self,
        _scope: PluginContextScope,
        id: IsolatedSessionId,
        content: CustomMessageContent,
    ) -> Result<IsolatedMessageReceipt, PluginContextError> {
        self.pi_session()?.send_to_isolated_session(&id, content)
    }

    async fn follow_up_isolated_session(
        &self,
        _scope: PluginContextScope,
        id: IsolatedSessionId,
        content: CustomMessageContent,
    ) -> Result<IsolatedFollowUpReceipt, PluginContextError> {
        self.pi_session()?
            .follow_up_isolated_session(&id, content)
            .await
    }

    fn abort_isolated_session_turn(
        &self,
        _scope: PluginContextScope,
        id: IsolatedSessionId,
        turn_id: IsolatedSessionTurnId,
    ) -> Result<(), PluginContextError> {
        self.pi_session()?
            .abort_isolated_session_turn(&id, &turn_id)
    }

    fn abort_isolated_session(
        &self,
        _scope: PluginContextScope,
        id: IsolatedSessionId,
    ) -> Result<(), PluginContextError> {
        self.pi_session()?.abort_isolated_session(&id)
    }

    async fn new_session(
        &self,
        scope: PluginContextScope,
        options: NewSessionOptions,
    ) -> Result<PluginContextReplacement, PluginContextError> {
        require_command(scope, "newSession")?;
        let pi_session = self.pi_session()?;
        let current = pi_session.current();
        let directory = current
            .store()
            .path()
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let path = directory.join(format!("{}.jsonl", uuid::Uuid::now_v7()));
        let replacement = match options.parent_session {
            Some(parent) => {
                pi_session
                    .new_session_with_parent(current.runtime().cwd(), path, parent)
                    .await
            }
            None => pi_session.new_session(current.runtime().cwd(), path).await,
        }
        .map_err(context_failed)?;
        Self::replacement(pi_session, scope, replacement)
    }

    async fn fork(
        &self,
        scope: PluginContextScope,
        entry_id: String,
        options: ForkOptions,
    ) -> Result<PluginContextReplacement, PluginContextError> {
        require_command(scope, "fork")?;
        let pi_session = self.pi_session()?;
        let replacement = pi_session
            .fork_session(entry_id, options.position)
            .await
            .map_err(context_failed)?;
        Self::replacement(pi_session, scope, replacement)
    }

    async fn navigate_tree(
        &self,
        scope: PluginContextScope,
        target_id: String,
        options: NavigateTreeOptions,
    ) -> Result<bool, PluginContextError> {
        require_command(scope, "navigateTree")?;
        let session = self.session()?;
        if options.summarize {
            session
                .summarize_branch_and_checkout(
                    &target_id,
                    options.custom_instructions,
                    options.replace_instructions,
                    options.label,
                )
                .await
                .map_err(context_failed)?;
        } else {
            session
                .checkout(Some(&target_id))
                .await
                .map_err(context_failed)?;
            if let Some(label) = options.label {
                session
                    .set_label(&target_id, Some(label))
                    .map_err(context_failed)?;
            }
        }
        Ok(false)
    }

    async fn switch_session(
        &self,
        scope: PluginContextScope,
        session_path: PathBuf,
    ) -> Result<PluginContextReplacement, PluginContextError> {
        require_command(scope, "switchSession")?;
        let pi_session = self.pi_session()?;
        let replacement = pi_session
            .resume_session(session_path)
            .await
            .map_err(context_failed)?;
        Self::replacement(pi_session, scope, replacement)
    }

    async fn reload(
        &self,
        scope: PluginContextScope,
    ) -> Result<PluginContextReplacement, PluginContextError> {
        require_command(scope, "reload")?;
        let pi_session = self.pi_session()?;
        pi_session.reload().await.map_err(context_failed)?;
        Ok(PluginContextReplacement {
            cancelled: false,
            context: Some(pi_session.current().runtime().plugin_context_handle(scope)),
        })
    }
}

fn session_entry_view(record: &SessionRecord) -> Result<SessionEntryView, PluginContextError> {
    let kind = match record.entry.entry_type() {
        SessionEntryType::Message => SessionEntryKind::Message,
        SessionEntryType::CustomMessage => SessionEntryKind::CustomMessage,
        SessionEntryType::ModelChange => SessionEntryKind::ModelChange,
        SessionEntryType::ThinkingLevelChange => SessionEntryKind::ThinkingLevelChange,
        SessionEntryType::ActiveToolsChange => SessionEntryKind::ActiveToolsChange,
        SessionEntryType::Compaction => SessionEntryKind::Compaction,
        SessionEntryType::BranchSummary => SessionEntryKind::BranchSummary,
        SessionEntryType::Custom => SessionEntryKind::Custom,
        SessionEntryType::ContextEdit => SessionEntryKind::ContextEdit,
    };
    Ok(SessionEntryView::new(
        record.id.clone(),
        record.parent_id.clone(),
        record.timestamp_ms,
        kind,
        value(record)?,
    ))
}

fn custom_delivery(
    message: CustomMessageInput,
    options: SendMessageOptions,
) -> crate::runtime::SessionDelivery {
    crate::runtime::SessionDelivery::Custom {
        message: message.into_message(crate::now_ms()),
        queue: options.deliver_as.map(delivery_queue),
        trigger_turn: options.trigger_turn,
    }
}

fn user_delivery(
    content: CustomMessageContent,
    options: SendUserMessageOptions,
) -> crate::runtime::SessionDelivery {
    crate::runtime::SessionDelivery::User {
        content,
        queue: options.deliver_as.map(delivery_queue),
        preprocess: options.expand_prompt_templates,
    }
}

fn delivery_queue(delivery: MessageDelivery) -> QueueKind {
    match delivery {
        MessageDelivery::FollowUp => QueueKind::FollowUp,
        MessageDelivery::NextTurn => QueueKind::NextRun,
        MessageDelivery::Steer => QueueKind::Steer,
    }
}

fn delivery_failed(error: crate::SessionError) -> PluginContextError {
    match error {
        crate::SessionError::InvalidPayload(message) => PluginContextError::Invalid(message),
        error => context_failed(error),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionTreeWireNode {
    entry: SessionRecord,
    children: Vec<SessionTreeWireNode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
}

fn session_tree(document: &SessionDocument) -> Vec<SessionTreeWireNode> {
    let ids = document
        .entries
        .iter()
        .map(|entry| entry.id.as_str())
        .collect::<HashSet<_>>();
    let mut children = HashMap::<Option<String>, Vec<SessionRecord>>::new();
    for entry in &document.entries {
        let parent = entry
            .parent_id
            .as_ref()
            .filter(|parent| parent.as_str() != entry.id && ids.contains(parent.as_str()))
            .cloned();
        children.entry(parent).or_default().push(entry.clone());
    }
    for entries in children.values_mut() {
        entries.sort_by_key(|entry| entry.timestamp_ms);
    }

    fn build(
        entry: SessionRecord,
        children: &mut HashMap<Option<String>, Vec<SessionRecord>>,
        labels: &HashMap<String, String>,
    ) -> SessionTreeWireNode {
        let descendants = children
            .remove(&Some(entry.id.clone()))
            .unwrap_or_default()
            .into_iter()
            .map(|child| build(child, children, labels))
            .collect();
        let label = labels.get(&entry.id).cloned();
        SessionTreeWireNode {
            entry,
            children: descendants,
            label,
        }
    }

    children
        .remove(&None)
        .unwrap_or_default()
        .into_iter()
        .map(|entry| build(entry, &mut children, &document.labels))
        .collect()
}
