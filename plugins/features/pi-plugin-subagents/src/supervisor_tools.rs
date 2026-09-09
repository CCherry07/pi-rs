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
pub(crate) const PARENT_GUIDANCE: &str = "Subagent supervision: a detached subagent is still alive. Answer pending requests using subagent_supervisor({action:\"reply\",replyTo:\"request-id\",message:\"...\"}); interview requests expect JSON in message. Then use bg_wait({id:\"run-id\"}) on that same run. bg_wait returns on completion, attention, or its wait-window timeout; a wait timeout does not stop the child. Resolve only decisions within the user's authorization; ask the user when needed. A detached receipt is not task completion.";

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
    action: String,
    to: Option<String>,
    message: Option<String>,
    reply_to: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WaitInput {
    id: Option<String>,
    #[serde(default)]
    all: bool,
    timeout_ms: Option<u64>,
    #[serde(default)]
    non_blocking: bool,
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
                "Reply to child requests or inspect pending supervisor requests. Reply before waiting for that child again.",
                json!({
                    "action":{"type":"string","enum":["list","pending","status","reply"]},
                    "to":{"type":"string"}, "message":{"type":"string"}, "replyTo":{"type":"string"}
                }),
                vec!["action"],
            ),
            SupervisorToolKind::Wait => (
                "bg_wait",
                "Wait for an owned detached subagent to finish or need attention. A timeout returns without stopping work. Use id for a specific run; otherwise wait for one active run, or all with all:true.",
                json!({
                    "id":{"type":"string"}, "all":{"type":"boolean"}, "timeoutMs":{"type":"integer","minimum":1},
                    "nonBlocking":{"type":"boolean","description":"Non-blocking subscriptions are not supported by this in-process runtime; use false."},
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
        let receiver = self
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
            details["delivered"] = json!(true);
        }
        Ok(result)
    }

    fn supervise(
        &self,
        context: ToolContext,
        input: SupervisorInput,
    ) -> Result<ToolResult, ToolError> {
        let owner = context.session.id()?;
        let pending = self.runtime.coordination().pending(&owner);
        match input.action.as_str() {
            "status" | "pending" | "list" => {
                let mut result = ToolResult::text(if pending.is_empty() {
                    "No pending supervisor requests.".into()
                } else {
                    serde_json::to_string_pretty(&pending).expect("serializable requests")
                });
                result.details = Some(if input.action == "status" {
                    json!({"active":true,"pending":pending.len()})
                } else {
                    json!({"pending":pending})
                });
                Ok(result)
            }
            "reply" => {
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
            _ => Err(ToolError::InvalidArguments(
                "Unsupported supervisor action.".into(),
            )),
        }
    }

    async fn wait(&self, context: ToolContext, input: WaitInput) -> Result<ToolResult, ToolError> {
        if input.non_blocking {
            return Err(ToolError::InvalidArguments("Non-blocking wait subscriptions are not supported by this in-process runtime. Use blocking bg_wait.".into()));
        }
        if input.timeout_ms == Some(0) {
            return Err(ToolError::InvalidArguments(
                "timeoutMs must be positive.".into(),
            ));
        }
        // Supervisor requests always break the wait, including stopOnAttention:false.
        let _ = input.stop_on_attention;
        let owner = context.session.id()?;
        let mut changed = self.runtime.coordination().subscribe();
        let ids = self
            .runtime
            .coordination()
            .run_ids(&owner, input.id.as_deref())
            .map_err(ToolError::Execution)?;
        let end = tokio::time::Instant::now()
            .checked_add(Duration::from_millis(input.timeout_ms.unwrap_or(1_800_000)))
            .ok_or_else(|| ToolError::InvalidArguments("timeoutMs is too large.".into()))?;
        let deadline = tokio::time::sleep_until(end);
        tokio::pin!(deadline);
        loop {
            let runs = ids
                .iter()
                .map(|id| self.runtime.coordination().run(&owner, id))
                .collect::<Result<Vec<_>, _>>()
                .map_err(ToolError::Execution)?;
            let pending = self.runtime.coordination().pending(&owner);
            let finished = runs.iter().filter(|run| run.result.is_some()).count();
            if !pending.is_empty() {
                let mut result = ToolResult::text(
                    "Subagent attention required. Reply to pending supervisor requests, then bg_wait on the same run. Do not launch a replacement.",
                );
                result.details =
                    Some(json!({"state":"needs_attention","pending":pending,"runIds":ids}));
                return Ok(result);
            }
            if runs.is_empty()
                || (input.all && finished == runs.len())
                || (!input.all && finished > 0)
            {
                if runs.len() == 1 {
                    return runs[0]
                        .result
                        .clone()
                        .expect("terminal run")
                        .map_err(ToolError::Execution);
                }
                let completed = runs
                    .iter()
                    .filter_map(|run| run.result.as_ref())
                    .map(|result| match result {
                        Ok(result) => json!({"content":result.content,"details":result.details,"isError":result.is_error}),
                        Err(error) => json!({"error":error}),
                    })
                    .collect::<Vec<_>>();
                let mut result = ToolResult::text(if runs.is_empty() {
                    "No active subagent runs."
                } else {
                    "Subagent wait completed."
                });
                result.details =
                    Some(json!({"state":"completed","results":completed,"runIds":ids}));
                return Ok(result);
            }
            tokio::select! {
                () = context.signal().wait() => return Err(ToolError::Aborted),
                _ = &mut deadline => {
                    let mut result = ToolResult::text("Wait window elapsed; subagent work keeps going. Call bg_wait again on the same run.");
                    result.details = Some(json!({"state":"running","timedOut":true,"runIds":ids}));
                    return Ok(result);
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
    use crate::coordination::ManagedRun;
    use pi_core::{
        AbortHandle, CustomMessageInput, ModelsContextAccess, PluginContextEpoch,
        PluginContextResult, SendMessageOptions, SessionContextAccess, UiContextAccess,
    };
    use std::sync::{Arc, Mutex};

    struct Access {
        id: String,
        messages: Mutex<Vec<CustomMessageInput>>,
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
            assert_eq!(options.trigger_turn, Some(true));
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
            ManagedRun {
                owner: "parent".into(),
                details: json!({"runId":ticket.run_id(),"agent":"reviewer"}),
                abort: abort.clone(),
                result: None,
                detached: false,
            },
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
        let receiver = runtime
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
            ManagedRun {
                owner: "parent".into(),
                details: json!({"runId":"second"}),
                abort,
                result: None,
                detached: false,
            },
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
