use std::time::Duration;

use async_trait::async_trait;
use pi_core::{
    Tool, ToolCallId, ToolContext, ToolError, ToolExecutionMode, ToolResult, ToolSpec,
    ToolUpdateSink,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::SubagentRuntime;
use crate::coordination::{SupervisorReason, SupervisorRequest, now_ms};

pub(crate) const CHILD_GUIDANCE: &str = "Intercom orchestration channel:\nUse contact_supervisor for product/API/scope decisions (reason: need_decision), structured input (reason: interview_request, with an interview object), or meaningful progress that changes the plan (reason: progress_update). The supervisor and run are resolved automatically. For decisions and interviews, wait for the tool's reply and continue the same task. Return ordinary completed work in your final response; completion needs no supervisor call. Treat inherited conversation as reference-only.";
pub(crate) const PARENT_GUIDANCE: &str = "Subagent supervision: background/detached receipts are not task completion. Use subagent_supervisor({action:\"status\",id:\"run-id\"}) for a read-only snapshot. Answer pending requests using subagent_supervisor({action:\"reply\",replyTo:\"request-id\",message:\"...\"}); interview requests expect JSON in message. Continue the same run, not a replacement. Completion and decision requests notify this session on a best-effort basis; subagent_supervisor status is authoritative. Use bg_wait({id:\"run-id\"}) if this turn needs the result. Use bg_wait({id:\"run-id\",nonBlocking:true,timeoutMs:30000}) to watch a detached run and continue working: it adds a one-shot expiry reminder, reuses completion/decision notifications, and repeated registration keeps the original deadline. Wait expiry does not cancel work. Resolve only decisions within the user's authorization; ask the user when needed.";

#[derive(Clone, Copy)]
pub(crate) enum SupervisorToolKind {
    Contact,
    Supervisor,
    Wait,
}

pub(crate) struct SupervisorTool {
    pub runtime: SubagentRuntime,
    pub kind: SupervisorToolKind,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContactInput {
    reason: SupervisorReason,
    message: Option<String>,
    interview: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SupervisorInput {
    action: SupervisorAction,
    id: Option<String>,
    to: Option<String>,
    message: Option<String>,
    reply_to: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum SupervisorAction {
    List,
    Pending,
    Status,
    Reply,
    Cancel,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WaitInput {
    id: Option<String>,
    #[serde(default)]
    all: bool,
    #[serde(default)]
    non_blocking: bool,
    timeout_ms: Option<u64>,
    stop_on_attention: Option<bool>,
}

#[async_trait]
impl Tool for SupervisorTool {
    fn spec(&self) -> ToolSpec {
        let (name, description, properties, required) = match self.kind {
            SupervisorToolKind::Contact => (
                "contact_supervisor",
                "Contact the parent/supervisor for a blocking decision, structured interview, or progress update. Available only to an assigned child.",
                json!({
                    "reason": {"type":"string","enum":["need_decision","interview_request","progress_update"]},
                    "message":{"type":"string"}, "interview":{"type":"object","additionalProperties":true}
                }),
                vec!["reason"],
            ),
            SupervisorToolKind::Supervisor => (
                "subagent_supervisor",
                "Inspect owned subagent/workflow status, cancel one owned run, list pending requests, or reply to a request. Reply before waiting for that child again.",
                json!({
                    "action":{"type":"string","enum":["list","pending","status","reply","cancel"]},
                    "id":{"type":"string","description":"For status or cancel: exact owned run id or unique prefix. Required for cancel."},
                    "to":{"type":"string"}, "message":{"type":"string"}, "replyTo":{"type":"string"}
                }),
                vec!["action"],
            ),
            SupervisorToolKind::Wait => (
                "bg_wait",
                "Wait for an owned subagent to finish or need attention. nonBlocking:true returns immediately and adds a one-shot expiry reminder for a single detached run. A timeout never stops work. Blocking waits also support the active set or all:true.",
                json!({
                    "id":{"type":"string"}, "all":{"type":"boolean"}, "timeoutMs":{"type":"integer","minimum":1},
                    "nonBlocking":{"type":"boolean","description":"Requires one id and forbids all:true. A detached run is watched in this process until completion, attention or wait expiry. Duplicate registration preserves the original deadline. Notifications are best effort."},
                    "stopOnAttention":{"type":"boolean"}
                }),
                vec![],
            ),
        };
        ToolSpec {
            name: name.into(),
            label: name.into(),
            description: description.into(),
            parameters: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
            execution_mode: ToolExecutionMode::Parallel,
            prompt_snippet: None,
            prompt_guidelines: vec![],
        }
    }

    async fn execute(
        &self,
        context: ToolContext,
        call_id: ToolCallId,
        input: Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        context.signal().check().map_err(|_| ToolError::Aborted)?;
        match self.kind {
            SupervisorToolKind::Contact => self.contact(context, call_id, parse(input)?).await,
            SupervisorToolKind::Supervisor => self.supervise(context, parse(input)?),
            SupervisorToolKind::Wait => self.wait(context, parse(input)?).await,
        }
    }
}

fn parse<T: serde::de::DeserializeOwned>(input: Value) -> Result<T, ToolError> {
    serde_json::from_value(input).map_err(|error| ToolError::InvalidArguments(error.to_string()))
}

impl SupervisorTool {
    async fn contact(
        &self,
        context: ToolContext,
        call_id: ToolCallId,
        input: ContactInput,
    ) -> Result<ToolResult, ToolError> {
        let message = input.message.unwrap_or_default().trim().to_string();
        if (message.is_empty() && input.reason != SupervisorReason::InterviewRequest)
            || input.interview.as_ref().is_some_and(|v| !v.is_object())
        {
            return Err(ToolError::InvalidArguments("A nonempty message is required for decisions/progress; interview must be an object.".into()));
        }
        let (run_id, profile) = self
            .runtime
            .assignment_for_session(&context.session.id()?)
            .ok_or_else(|| {
                ToolError::Execution(
                    "contact_supervisor requires a live assigned child session.".into(),
                )
            })?;
        let timeout = std::env::var("PI_INTERCOM_ASK_TIMEOUT_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(600_000);
        let expects_reply = input.reason != SupervisorReason::ProgressUpdate;
        let request = SupervisorRequest {
            id: uuid::Uuid::now_v7().to_string(),
            run_id,
            agent: profile.name,
            child_index: 0,
            tool_call_id: call_id.to_string(),
            reason: input.reason,
            message,
            expects_reply,
            created_at: now_ms(),
            expires_at: expects_reply.then(|| now_ms().saturating_add(timeout)),
            interview: input.interview,
        };
        if serde_json::to_vec(&request)
            .expect("serializable request")
            .len()
            > 64 * 1024
        {
            return Err(ToolError::InvalidArguments(
                "Supervisor request exceeds 64 KiB.".into(),
            ));
        }
        let (receiver, delivered) = self
            .runtime
            .coordination()
            .post(request.clone(), Duration::from_millis(timeout))
            .map_err(ToolError::Execution)?;
        let mut result = if !expects_reply {
            ToolResult::text("Supervisor progress update queued.")
        } else {
            let _guard = RequestGuard {
                runtime: self.runtime.clone(),
                id: request.id.clone(),
            };
            let reply = tokio::select! {
                biased;
                () = context.signal().wait() => return Err(ToolError::Aborted),
                reply = receiver => reply.map_err(|_| ToolError::Execution("Supervisor request is no longer active.".into()))?.map_err(ToolError::Execution)?,
                () = tokio::time::sleep(Duration::from_millis(timeout)) => return Err(ToolError::Execution("Timed out waiting for supervisor reply.".into())),
            };
            let mut result = ToolResult::text(format!("**Reply from supervisor:**\n{reply}"));
            if input.reason == SupervisorReason::InterviewRequest {
                result.details = Some(match parse_structured_reply(&reply) {
                    Ok(value) => json!({"structuredReply":value}),
                    Err(error) => json!({"structuredReplyParseError":error.to_string()}),
                });
            }
            result
        };
        let details = result.details.get_or_insert_with(|| json!({}));
        details["requestId"] = json!(request.id);
        details["reason"] = json!(input.reason);
        if !expects_reply {
            details["queued"] = json!(true);
            details["delivered"] = json!(delivered);
        }
        Ok(result)
    }

    fn supervise(
        &self,
        context: ToolContext,
        input: SupervisorInput,
    ) -> Result<ToolResult, ToolError> {
        let owner = context.session.id()?;
        if input.id.is_some()
            && !matches!(
                input.action,
                SupervisorAction::Status | SupervisorAction::Cancel
            )
        {
            return Err(ToolError::InvalidArguments(
                "id is only supported for status or cancel.".into(),
            ));
        }
        if input.action == SupervisorAction::Cancel {
            let prefix = input
                .id
                .as_deref()
                .filter(|id| !id.trim().is_empty())
                .ok_or_else(|| {
                    ToolError::InvalidArguments("cancel requires one owned run id.".into())
                })?;
            let ids = self
                .runtime
                .coordination()
                .run_ids(&owner, Some(prefix))
                .map_err(ToolError::Execution)?;
            self.runtime
                .coordination()
                .cancel(&owner, &ids[0])
                .map_err(ToolError::Execution)?;
        }
        if matches!(
            input.action,
            SupervisorAction::Status | SupervisorAction::Cancel
        ) {
            let status = self
                .runtime
                .coordination()
                .status(&owner, input.id.as_deref())
                .map_err(ToolError::Execution)?;
            let mut result =
                ToolResult::text(serde_json::to_string_pretty(&status).expect("status"));
            result.details = Some(status);
            return Ok(result);
        }
        let pending = self.runtime.coordination().pending(&owner);
        match input.action {
            SupervisorAction::Pending | SupervisorAction::List => {
                let mut result = ToolResult::text(if pending.is_empty() {
                    "No pending supervisor requests.".into()
                } else {
                    serde_json::to_string_pretty(&pending).expect("serializable requests")
                });
                result.details = Some(json!({"pending":pending}));
                Ok(result)
            }
            SupervisorAction::Reply => {
                let message = input.message.unwrap_or_default();
                let request = self
                    .runtime
                    .coordination()
                    .reply(
                        &owner,
                        input.reply_to.as_deref(),
                        input.to.as_deref(),
                        &message,
                    )
                    .map_err(ToolError::Execution)?;
                // Delivery is authoritative, just as upstream's reply file is.
                let journal = context.session.append_entry("subagent_supervisor_reply", Some(json!({"requestId":request.id,"reason":request.reason,"runId":request.run_id,"agent":request.agent,"childIndex":request.child_index,"message":message.trim(),"createdAt":now_ms()})));
                let mut result =
                    ToolResult::text(format!("Replied to supervisor request {}.", request.id));
                result.details = Some(
                    json!({"replyTo":request.id,"runId":request.run_id,"agent":request.agent}),
                );
                if let Err(error) = journal {
                    result.details.as_mut().unwrap()["journalWarning"] = json!(error.to_string());
                }
                Ok(result)
            }
            SupervisorAction::Status | SupervisorAction::Cancel => {
                unreachable!("status/cancel handled above")
            }
        }
    }

    async fn wait(&self, context: ToolContext, input: WaitInput) -> Result<ToolResult, ToolError> {
        if input.timeout_ms == Some(0) {
            return Err(ToolError::InvalidArguments(
                "timeoutMs must be positive.".into(),
            ));
        }
        if input.non_blocking
            && (input.all || input.id.as_deref().is_none_or(|id| id.trim().is_empty()))
        {
            return Err(ToolError::InvalidArguments(
                "nonBlocking requires one id and cannot be combined with all:true.".into(),
            ));
        }
        // Supervisor requests always break the wait, including stopOnAttention:false.
        let _ = input.stop_on_attention;
        let owner = context.session.id()?;
        let ids = self
            .runtime
            .coordination()
            .run_ids(&owner, input.id.as_deref())
            .map_err(ToolError::Execution)?;
        let timeout = Duration::from_millis(input.timeout_ms.unwrap_or(1_800_000));
        let end = tokio::time::Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| ToolError::InvalidArguments("timeoutMs is too large.".into()))?;
        if input.non_blocking {
            return self
                .runtime
                .coordination()
                .arm_wait(&owner, &ids[0], timeout)
                .map_err(ToolError::Execution);
        }
        let mut changed = self.runtime.coordination().subscribe();
        let deadline = tokio::time::sleep_until(end);
        tokio::pin!(deadline);
        loop {
            if let crate::waiting::WaitDecision::Ready(result) = self
                .runtime
                .coordination()
                .wait_decision(&owner, &ids, input.all)
                .map_err(ToolError::Execution)?
            {
                return (*result).map_err(ToolError::Execution);
            }
            tokio::select! {
                () = context.signal().wait() => return Err(ToolError::Aborted),
                _ = &mut deadline => {
                    if let crate::waiting::WaitDecision::Ready(result) = self
                        .runtime
                        .coordination()
                        .wait_decision(&owner, &ids, input.all)
                        .map_err(ToolError::Execution)?
                    {
                        return (*result).map_err(ToolError::Execution);
                    }
                    return Ok(crate::waiting::window_elapsed(&ids));
                }
                _ = changed.changed() => {}
            }
        }
    }
}

struct RequestGuard {
    runtime: SubagentRuntime,
    id: String,
}
impl Drop for RequestGuard {
    fn drop(&mut self) {
        self.runtime.coordination().withdraw(&self.id);
    }
}

fn parse_structured_reply(reply: &str) -> Result<Value, serde_json::Error> {
    let text = reply.trim();
    let text = if let Some(fenced) = text
        .strip_prefix("```")
        .and_then(|rest| rest.strip_suffix("```"))
    {
        if fenced
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("json"))
        {
            fenced[4..].trim()
        } else {
            fenced.trim()
        }
    } else {
        text
    };
    serde_json::from_str(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordination::{ManagedRun, RunMetadata};
    use pi_core::{
        AbortHandle, CustomMessageInput, IsolatedContextMode, ModelsContextAccess,
        PluginContextEpoch, PluginContextResult, SendMessageOptions, SessionContextAccess,
        UiContextAccess,
    };
    use std::sync::{Arc, Mutex};

    struct Access {
        id: String,
        messages: Mutex<Vec<CustomMessageInput>>,
        trigger_turns: Mutex<Vec<Option<bool>>>,
    }
    #[async_trait]
    impl SessionContextAccess for Access {
        fn session_id(&self) -> PluginContextResult<String> {
            Ok(self.id.clone())
        }
        fn send_message(
            &self,
            message: CustomMessageInput,
            options: SendMessageOptions,
        ) -> PluginContextResult<()> {
            self.trigger_turns
                .lock()
                .unwrap()
                .push(options.trigger_turn);
            self.messages.lock().unwrap().push(message);
            Ok(())
        }
        fn append_entry(&self, _kind: String, _data: Option<Value>) -> PluginContextResult<()> {
            Ok(())
        }
    }
    #[async_trait]
    impl ModelsContextAccess for Access {}
    #[async_trait]
    impl UiContextAccess for Access {}

    fn access(id: &str) -> (Arc<Access>, PluginContextEpoch) {
        let access = Arc::new(Access {
            id: id.into(),
            messages: Mutex::new(vec![]),
            trigger_turns: Mutex::new(vec![]),
        });
        let epoch = PluginContextEpoch::new(access.clone());
        (access, epoch)
    }

    fn setup() -> (
        SubagentRuntime,
        String,
        Arc<Access>,
        PluginContextEpoch,
        PluginContextEpoch,
        AbortHandle,
    ) {
        let runtime = SubagentRuntime::default();
        let ticket = runtime
            .begin_launch("parent", crate::profiles::builtin_profile("reviewer"))
            .unwrap();
        runtime.bind_child(ticket.run_id(), "child").unwrap();
        let (parent, parent_epoch) = access("parent");
        let (_, child_epoch) = access("child");
        runtime.coordination().bind_session(
            "parent".into(),
            parent_epoch.context().session.handle_for_adapter(),
        );
        let (abort, _) = AbortHandle::new();
        runtime.coordination().reserve(
            ticket.run_id(),
            ManagedRun::new(
                "parent".into(),
                RunMetadata::new("reviewer".into(), 1, IsolatedContextMode::Fresh),
                abort.clone(),
                None,
            ),
        );
        (
            runtime,
            ticket.run_id().into(),
            parent,
            parent_epoch,
            child_epoch,
            abort,
        )
    }

    async fn call(
        runtime: SubagentRuntime,
        epoch: &PluginContextEpoch,
        kind: SupervisorToolKind,
        input: Value,
    ) -> Result<ToolResult, ToolError> {
        let (_, signal) = AbortHandle::new();
        let context = ToolContext::with_plugin_context(".".into(), signal, epoch.context());
        SupervisorTool { runtime, kind }
            .execute(
                context,
                ToolCallId::new("test-call"),
                input,
                ToolUpdateSink::channel().0,
            )
            .await
    }

    async fn pending(runtime: &SubagentRuntime) -> SupervisorRequest {
        let mut changed = runtime.coordination().subscribe();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(request) = runtime.coordination().pending("parent").into_iter().next() {
                    return request;
                }
                changed.changed().await.unwrap();
            }
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn decision_reply_is_owner_scoped_exactly_once_and_resumes_the_call() {
        let (runtime, _, parent, parent_epoch, child_epoch, _) = setup();
        let child_runtime = runtime.clone();
        let child = tokio::spawn(async move {
            call(
                child_runtime,
                &child_epoch,
                SupervisorToolKind::Contact,
                json!({"reason":"need_decision","message":"Keep compatibility?"}),
            )
            .await
        });
        let request = pending(&runtime).await;
        assert!(!child.is_finished());
        assert_eq!(
            parent.messages.lock().unwrap()[0].custom_type,
            "subagent_supervisor_request"
        );
        assert!(
            runtime
                .coordination()
                .reply("other", Some(&request.id), None, "yes")
                .is_err()
        );
        let reply = json!({"action":"reply","replyTo":request.id,"message":"yes"});
        call(
            runtime.clone(),
            &parent_epoch,
            SupervisorToolKind::Supervisor,
            reply.clone(),
        )
        .await
        .unwrap();
        assert!(
            call(
                runtime.clone(),
                &parent_epoch,
                SupervisorToolKind::Supervisor,
                reply
            )
            .await
            .is_err()
        );
        let result = child.await.unwrap().unwrap();
        assert_eq!(result.details.unwrap()["requestId"], request.id);
        assert!(runtime.coordination().pending("parent").is_empty());
    }

    #[tokio::test]
    async fn progress_is_nonblocking_and_cannot_be_replied_to() {
        let (runtime, _, parent, _, child_epoch, _) = setup();
        let result = call(
            runtime.clone(),
            &child_epoch,
            SupervisorToolKind::Contact,
            json!({"reason":"progress_update","message":"Unexpected compatibility dependency"}),
        )
        .await
        .unwrap();
        assert_eq!(result.details.unwrap()["delivered"], true);
        assert_eq!(parent.messages.lock().unwrap().len(), 1);
        assert_eq!(*parent.trigger_turns.lock().unwrap(), vec![Some(false)]);
        assert!(runtime.coordination().pending("parent").is_empty());
    }

    #[tokio::test]
    async fn cancelled_contact_withdraws_request_and_close_releases_waiters() {
        let (runtime, _, _, _, child_epoch, abort) = setup();
        let child_runtime = runtime.clone();
        let child_epoch_clone = child_epoch.clone();
        let child = tokio::spawn(async move {
            call(
                child_runtime,
                &child_epoch_clone,
                SupervisorToolKind::Contact,
                json!({"reason":"need_decision","message":"Choose"}),
            )
            .await
        });
        pending(&runtime).await;
        child.abort();
        let _ = child.await;
        assert!(runtime.coordination().pending("parent").is_empty());
        let child_runtime = runtime.clone();
        let child = tokio::spawn(async move {
            call(
                child_runtime,
                &child_epoch,
                SupervisorToolKind::Contact,
                json!({"reason":"need_decision","message":"Choose again"}),
            )
            .await
        });
        pending(&runtime).await;
        runtime.forget_session("parent");
        assert!(abort.is_aborted());
        assert!(child.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn wait_timeout_does_not_cancel_and_named_wait_returns_retained_result() {
        let (runtime, id, _, parent_epoch, _, abort) = setup();
        let result = call(
            runtime.clone(),
            &parent_epoch,
            SupervisorToolKind::Wait,
            json!({"id":id,"timeoutMs":1}),
        )
        .await
        .unwrap();
        assert_eq!(result.details.unwrap()["timedOut"], true);
        assert!(!abort.is_aborted());
        runtime
            .coordination()
            .complete(&id, Ok(ToolResult::text("finished")));
        for _ in 0..2 {
            let result = call(
                runtime.clone(),
                &parent_epoch,
                SupervisorToolKind::Wait,
                json!({"id":id}),
            )
            .await
            .unwrap();
            assert_eq!(result.content, ToolResult::text("finished").content);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn nonblocking_wait_reuses_deadline_expires_once_and_leaves_run_alive() {
        let (runtime, id, parent, epoch, _, abort) = setup();
        runtime.coordination().launched(&id, "isolated");
        runtime.coordination().background("parent", &id).unwrap();
        let first = call(
            runtime.clone(),
            &epoch,
            SupervisorToolKind::Wait,
            json!({"id": &id[..8], "nonBlocking":true,"timeoutMs":1000}),
        )
        .await
        .unwrap();
        let receipt = first.details.unwrap();
        assert_eq!(receipt["runId"], id);
        assert_eq!(receipt["armed"], true);
        assert_eq!(receipt["reused"], false);
        tokio::time::advance(Duration::from_millis(500)).await;
        let duplicate = call(
            runtime.clone(),
            &epoch,
            SupervisorToolKind::Wait,
            json!({"id":id,"nonBlocking":true,"timeoutMs":9000}),
        )
        .await
        .unwrap();
        let duplicate = duplicate.details.unwrap();
        assert_eq!(duplicate["reused"], true);
        assert_eq!(duplicate["deadlineAt"], receipt["deadlineAt"]);
        let status = call(
            runtime.clone(),
            &epoch,
            SupervisorToolKind::Supervisor,
            json!({"action":"status","id":id}),
        )
        .await
        .unwrap()
        .details
        .unwrap();
        assert_eq!(
            status["runs"][0]["nonBlockingWait"]["deadlineAt"],
            receipt["deadlineAt"]
        );
        tokio::time::sleep(Duration::from_millis(501)).await;
        let messages = parent.messages.lock().unwrap().clone();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].custom_type, "subagent-wait-expired");
        assert_eq!(messages[0].details.as_ref().unwrap()["runId"], id);
        assert_eq!(messages[0].details.as_ref().unwrap()["state"], "running");
        assert_eq!(*parent.trigger_turns.lock().unwrap(), vec![Some(true)]);
        assert!(!abort.is_aborted());
        let status = runtime.coordination().status("parent", Some(&id)).unwrap();
        assert_eq!(status["runs"][0]["state"], "running");
        assert!(status["runs"][0].get("nonBlockingWait").is_none());
        tokio::time::sleep(Duration::from_secs(20)).await;
        assert_eq!(parent.messages.lock().unwrap().len(), 1);
        runtime
            .coordination()
            .complete(&id, Ok(ToolResult::text("done later")));
        assert_eq!(
            parent.messages.lock().unwrap()[1].custom_type,
            "subagent-notify"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn nonblocking_wait_completion_and_attention_reuse_existing_notifications() {
        for attention in [false, true] {
            let (runtime, id, parent, epoch, child_epoch, _) = setup();
            runtime.coordination().background("parent", &id).unwrap();
            call(
                runtime.clone(),
                &epoch,
                SupervisorToolKind::Wait,
                json!({"id":id,"nonBlocking":true,"timeoutMs":1000}),
            )
            .await
            .unwrap();
            let request = if attention {
                let child_runtime = runtime.clone();
                Some(tokio::spawn(async move {
                    call(
                        child_runtime,
                        &child_epoch,
                        SupervisorToolKind::Contact,
                        json!({"reason":"need_decision","message":"Choose"}),
                    )
                    .await
                }))
            } else {
                runtime
                    .coordination()
                    .complete(&id, Ok(ToolResult::text("done")));
                None
            };
            if attention {
                pending(&runtime).await;
            }
            let ready = call(
                runtime.clone(),
                &epoch,
                SupervisorToolKind::Wait,
                json!({"id":id,"nonBlocking":true}),
            )
            .await
            .unwrap();
            if attention {
                assert_eq!(ready.details.unwrap()["state"], "needs_attention");
            } else {
                assert_eq!(ready.content, ToolResult::text("done").content);
            }
            let status = runtime.coordination().status("parent", Some(&id)).unwrap();
            assert!(status["runs"][0].get("nonBlockingWait").is_none());
            tokio::time::sleep(Duration::from_secs(2)).await;
            let messages = parent.messages.lock().unwrap().clone();
            assert_eq!(messages.len(), 1);
            assert_eq!(
                messages[0].custom_type,
                if attention {
                    "subagent_supervisor_request"
                } else {
                    "subagent-notify"
                }
            );
            runtime.forget_session("parent");
            if let Some(request) = request {
                assert!(request.await.unwrap().is_err());
            }
        }
    }

    #[tokio::test]
    async fn nonblocking_wait_rejects_invalid_selection_without_registering() {
        let (runtime, id, _, epoch, _, _) = setup();
        for input in [
            json!({"nonBlocking":true}),
            json!({"id":" ","nonBlocking":true}),
            json!({"id":id,"all":true,"nonBlocking":true}),
            json!({"id":id,"timeoutMs":0,"nonBlocking":true}),
            json!({"id":"unknown","nonBlocking":true}),
            json!({"id":id,"nonBlocking":true}), // still foreground
        ] {
            assert!(
                call(runtime.clone(), &epoch, SupervisorToolKind::Wait, input)
                    .await
                    .is_err()
            );
        }
        runtime.coordination().background("parent", &id).unwrap();
        let (_, foreign_epoch) = access("foreign");
        assert!(
            call(
                runtime.clone(),
                &foreign_epoch,
                SupervisorToolKind::Wait,
                json!({"id":id,"nonBlocking":true})
            )
            .await
            .is_err()
        );
        assert!(
            runtime.coordination().status("parent", Some(&id)).unwrap()["runs"][0]
                .get("nonBlockingWait")
                .is_none()
        );
    }

    #[tokio::test]
    async fn wait_breaks_on_supervisor_attention_even_when_disabled() {
        let (runtime, id, _, parent_epoch, child_epoch, abort) = setup();
        let child_runtime = runtime.clone();
        let child = tokio::spawn(async move {
            call(
                child_runtime,
                &child_epoch,
                SupervisorToolKind::Contact,
                json!({"reason":"need_decision","message":"Choose"}),
            )
            .await
        });
        pending(&runtime).await;
        let result = call(
            runtime.clone(),
            &parent_epoch,
            SupervisorToolKind::Wait,
            json!({"id":id,"stopOnAttention":false}),
        )
        .await
        .unwrap();
        assert_eq!(result.details.unwrap()["state"], "needs_attention");
        assert!(!abort.is_aborted());
        runtime.forget_session("parent");
        let _ = child.await;
    }

    #[tokio::test]
    async fn contact_rejects_primary_sessions_and_invalid_payloads() {
        let (runtime, _, _, parent_epoch, child_epoch, _) = setup();
        assert!(
            call(
                runtime.clone(),
                &parent_epoch,
                SupervisorToolKind::Contact,
                json!({"reason":"need_decision","message":"choose"})
            )
            .await
            .is_err()
        );
        for input in [
            json!({"reason":"other"}),
            json!({"reason":"need_decision","message":" "}),
            json!({"reason":"interview_request","interview":[]}),
        ] {
            assert!(
                call(
                    runtime.clone(),
                    &child_epoch,
                    SupervisorToolKind::Contact,
                    input
                )
                .await
                .is_err()
            );
        }
    }

    #[test]
    fn structured_replies_accept_json_and_fences_and_report_parse_failure() {
        assert_eq!(
            parse_structured_reply("```json\n{\"approved\":true}\n```").unwrap(),
            json!({"approved":true})
        );
        assert_eq!(parse_structured_reply("[1,2]").unwrap(), json!([1, 2]));
        assert!(parse_structured_reply("not json").is_err());
        assert_eq!(
            parse_structured_reply("```JSON {\"a\":1}```").unwrap(),
            json!({"a":1})
        );
        assert!(parse_structured_reply("```python\n{}\n```").is_err());
    }

    #[tokio::test]
    async fn expired_requests_reject_late_replies_and_release_receivers() {
        let (runtime, run_id, _, _, _, _) = setup();
        let request = SupervisorRequest {
            id: "expired".into(),
            run_id,
            agent: "reviewer".into(),
            child_index: 0,
            tool_call_id: "ask".into(),
            reason: SupervisorReason::NeedDecision,
            message: "Choose".into(),
            expects_reply: true,
            created_at: now_ms(),
            expires_at: Some(now_ms()),
            interview: None,
        };
        let (receiver, _) = runtime
            .coordination()
            .post(request, Duration::ZERO)
            .unwrap();
        assert!(
            runtime
                .coordination()
                .reply("parent", Some("expired"), None, "late")
                .is_err()
        );
        assert!(receiver.await.is_err());
    }

    #[tokio::test]
    async fn all_wait_keeps_waiting_until_every_snapshotted_run_finishes() {
        let (runtime, first, _, parent_epoch, _, _) = setup();
        let (abort, _) = AbortHandle::new();
        runtime.coordination().reserve(
            "second",
            ManagedRun::new(
                "parent".into(),
                RunMetadata::new("reviewer".into(), 1, IsolatedContextMode::Fresh),
                abort,
                None,
            ),
        );
        let wait_runtime = runtime.clone();
        let wait = tokio::spawn(async move {
            call(
                wait_runtime,
                &parent_epoch,
                SupervisorToolKind::Wait,
                json!({"all":true,"timeoutMs":1000}),
            )
            .await
        });
        // It is immaterial whether the wait snapshots before or after the first
        // completion: the second run remains live in either case.
        runtime
            .coordination()
            .complete(&first, Ok(ToolResult::text("one")));
        tokio::task::yield_now().await;
        assert!(!wait.is_finished());
        runtime
            .coordination()
            .complete("second", Ok(ToolResult::text("two")));
        assert!(wait.await.unwrap().is_ok());
    }
}
