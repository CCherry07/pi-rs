use std::sync::Arc;

use pi_core::{ContentBlock, Message};
use pi_rpc::json_wire::{session_event_json, session_header_json};
use pi_session::{
    AgentSessionEvent, PiSession, RevisionedAgentSessionEvent, ShellExecutionOptions, SubmitOutcome,
};
use serde_json::Value;

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
    let mut subscription = session.subscribe();
    let submission = session.submit(input);
    tokio::pin!(submission);
    let outcome = loop {
        tokio::select! {
            result = &mut submission => {
                let outcome = result.map_err(|error| error.to_string())?;
                while let Ok(event) = subscription.events.try_recv() {
                    print_extension_notice(&subscription, event);
                }
                break outcome;
            }
            event = subscription.events.recv() => match event {
                Ok(event) => print_extension_notice(&subscription, event),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    subscription.snapshot = session.snapshot();
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    return Err("session event stream closed during print submission".to_string());
                }
            }
        }
    };
    match outcome {
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

fn print_extension_notice(
    subscription: &pi_session::AgentSessionSubscription,
    event: RevisionedAgentSessionEvent,
) {
    if event.revision <= subscription.snapshot.revision {
        return;
    }
    if let AgentSessionEvent::ExtensionNotice { message, .. } = event.event {
        println!("{message}");
    }
}

pub(crate) async fn run_json(session_handle: PiSession, input: String) -> Result<(), String> {
    let session = session_handle.current();
    let mut subscription = session.subscribe();
    emit(session_header_json(&session.log().header())?);
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
                    emit_wire_event(&session, event)?;
                }
                break;
            }
            event = subscription.events.recv() => match event {
                Ok(event) if event.revision > subscription.snapshot.revision => emit_wire_event(&session, event)?,
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    return Err("session event stream lagged during JSON output".to_string());
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    }
    Ok(())
}

fn emit_wire_event(
    session: &pi_session::AgentSession,
    event: RevisionedAgentSessionEvent,
) -> Result<(), String> {
    if let Some(value) = session_event_json(event.event, session)? {
        emit(value);
    }
    Ok(())
}

fn emit(value: Value) {
    println!(
        "{}",
        serde_json::to_string(&value).expect("JSON event serialization")
    );
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
