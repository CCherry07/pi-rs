use std::sync::Arc;

use pi_core::{AgentEvent, ContentBlock, Message};
use pi_session::{
    AgentSessionEvent, PiSession, RevisionedAgentSessionEvent, ShellExecutionOptions, SubmitOutcome,
};
use serde_json::{Value, json};

pub(crate) async fn run_print(session_handle: PiSession, input: String) -> Result<(), String> {
    let session = session_handle.current();
    if let Some((command, excluded)) = shell_command(&input) {
        let result = session
            .execute_shell(
                command,
                ShellExecutionOptions {
                    exclude_from_context: excluded,
                    ..ShellExecutionOptions::default()
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        print!("{}", result.output);
        if !result.output.ends_with('\n') {
            println!();
        }
        return Ok(());
    }
    match session
        .submit(input)
        .await
        .map_err(|error| error.to_string())?
    {
        SubmitOutcome::Agent(outcome) => {
            if let Some(text) = outcome.new_messages.iter().rev().find_map(assistant_text) {
                print!("{text}");
                if !text.ends_with('\n') {
                    println!();
                }
            }
        }
        SubmitOutcome::Handled => {}
        SubmitOutcome::Queued { .. } => {
            return Err("print mode cannot leave a queued prompt".to_string());
        }
        _ => return Err("unsupported submission outcome".to_string()),
    }
    Ok(())
}

pub(crate) async fn run_json(session_handle: PiSession, input: String) -> Result<(), String> {
    let session = session_handle.current();
    let mut subscription = session.subscribe();
    emit(json!({"type":"snapshot","snapshot": snapshot_json(&subscription.snapshot)}));
    let worker = {
        let session = Arc::clone(&session);
        tokio::spawn(async move {
            if let Some((command, excluded)) = shell_command(&input) {
                session
                    .execute_shell(
                        command,
                        ShellExecutionOptions {
                            exclude_from_context: excluded,
                            ..ShellExecutionOptions::default()
                        },
                    )
                    .await
                    .map(|_| ())
            } else {
                session.submit(input).await.map(|_| ())
            }
        })
    };
    tokio::pin!(worker);
    loop {
        tokio::select! {
            result = &mut worker => {
                result.map_err(|error| error.to_string())?
                    .map_err(|error| error.to_string())?;
                while let Ok(event) = subscription.events.try_recv() {
                    emit(event_json(event));
                }
                break;
            }
            event = subscription.events.recv() => match event {
                Ok(event) if event.revision > subscription.snapshot.revision => emit(event_json(event)),
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    let snapshot = session.snapshot();
                    subscription.snapshot = snapshot;
                    emit(json!({"type":"snapshot","reason":"lagged","snapshot":snapshot_json(&subscription.snapshot)}));
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    }
    Ok(())
}

fn emit(value: Value) {
    println!(
        "{}",
        serde_json::to_string(&value).expect("JSON event serialization")
    );
}

fn snapshot_json(snapshot: &pi_session::AgentSessionSnapshot) -> Value {
    json!({
        "revision": snapshot.revision,
        "running": snapshot.agent.is_running,
        "provider": snapshot.agent.provider_id,
        "model": snapshot.agent.model_id,
        "thinkingLevel": snapshot.agent.thinking_level,
        "queue": {"steering":snapshot.queue.steering,"followUp":snapshot.queue.follow_up},
        "compacting": snapshot.compaction.is_some(),
        "bashRunning": snapshot.bash.is_some(),
        "name": snapshot.name,
    })
}

fn event_json(event: RevisionedAgentSessionEvent) -> Value {
    let payload = match event.event {
        AgentSessionEvent::Agent(agent) => agent_event_json(*agent),
        AgentSessionEvent::AgentSettled => json!({"type":"agent_settled"}),
        AgentSessionEvent::QueueUpdate {
            steering,
            follow_up,
        } => {
            json!({"type":"queue_update","steering":steering,"followUp":follow_up})
        }
        AgentSessionEvent::CompactionStart { reason } => {
            json!({"type":"compaction_start","reason":format!("{reason:?}").to_ascii_lowercase()})
        }
        AgentSessionEvent::CompactionEnd {
            aborted,
            will_retry,
            error_message,
            ..
        } => {
            json!({"type":"compaction_end","aborted":aborted,"willRetry":will_retry,"error":error_message})
        }
        AgentSessionEvent::EntryAppended { entry } => {
            json!({"type":"entry_appended","entry":entry})
        }
        AgentSessionEvent::SessionInfoChanged { name } => {
            json!({"type":"session_info_changed","name":name})
        }
        AgentSessionEvent::ThinkingLevelChanged { level } => {
            json!({"type":"thinking_level_changed","level":level})
        }
        AgentSessionEvent::BashExecutionStart {
            id,
            command,
            exclude_from_context,
        } => {
            json!({"type":"bash_execution_start","id":id,"command":command,"excludeFromContext":exclude_from_context})
        }
        AgentSessionEvent::BashExecutionUpdate { id, stream, delta } => {
            json!({"type":"bash_execution_update","id":id,"stream":format!("{stream:?}").to_ascii_lowercase(),"delta":delta})
        }
        AgentSessionEvent::BashExecutionEnd {
            id,
            result,
            error_message,
        } => {
            json!({"type":"bash_execution_end","id":id,"result":result.map(|result| json!({
                "output":result.output,"exitCode":result.exit_code,"cancelled":result.cancelled,
                "timedOut":result.timed_out,"truncated":result.truncated,
                "fullOutputPath":result.full_output_path
            })),"error":error_message})
        }
        _ => json!({"type":"unknown"}),
    };
    json!({"revision":event.revision,"event":payload})
}

fn agent_event_json(event: AgentEvent) -> Value {
    match event {
        AgentEvent::AgentStart => json!({"type":"agent_start"}),
        AgentEvent::AgentEnd { messages } => json!({"type":"agent_end","messages":messages}),
        AgentEvent::TurnStart => json!({"type":"turn_start"}),
        AgentEvent::TurnEnd {
            message,
            tool_results,
        } => {
            json!({"type":"turn_end","message":message,"toolResults":tool_results})
        }
        AgentEvent::MessageStart { message } => json!({"type":"message_start","message":message}),
        AgentEvent::MessageUpdate { message, event } => {
            json!({"type":"message_update","message":message,"update":format!("{event:?}")})
        }
        AgentEvent::MessageEnd { message } => json!({"type":"message_end","message":message}),
        AgentEvent::ToolExecutionStart {
            tool_call_id,
            tool_name,
            args,
        } => {
            json!({"type":"tool_execution_start","toolCallId":tool_call_id,"toolName":tool_name,"args":args})
        }
        AgentEvent::ToolExecutionUpdate {
            tool_call_id,
            tool_name,
            partial_result,
            ..
        } => {
            json!({"type":"tool_execution_update","toolCallId":tool_call_id,"toolName":tool_name,"isError":partial_result.is_error})
        }
        AgentEvent::ToolExecutionEnd {
            tool_call_id,
            tool_name,
            result,
            is_error,
        } => {
            json!({
                "type":"tool_execution_end","toolCallId":tool_call_id,"toolName":tool_name,
                "result":{"content":result.content,"details":result.details,"isError":result.is_error,"terminate":result.terminate},
                "isError":is_error
            })
        }
    }
}

pub(crate) fn assistant_text(message: &Message) -> Option<String> {
    let Message::Assistant(message) = message else {
        return None;
    };
    Some(
        message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(""),
    )
}

pub(crate) fn shell_command(input: &str) -> Option<(String, bool)> {
    let excluded = input.starts_with("!!");
    let command = if excluded {
        input.strip_prefix("!!")?
    } else {
        input.strip_prefix('!')?
    }
    .trim();
    (!command.is_empty()).then(|| (command.to_string(), excluded))
}
