//! Ordered translation of Agent observations into durable product events.
use std::sync::Arc;

use pi_agent::EventError;
use pi_core::{AgentEvent, ContentBlock, Message, UserMessage};

use super::AgentSession;
use super::run::will_retry_after_agent_end;

pub(super) fn attach(session: &AgentSession) {
    let events = Arc::clone(&session.events);
    let agent = session.runtime.agent().downgrade();
    let activity = Arc::clone(&session.activity);
    let retry_settings = Arc::clone(&session.retry_settings);
    session.runtime.agent().subscribe(Arc::new(
        move |event: AgentEvent, _: pi_core::AbortSignal| {
            let events = Arc::clone(&events);
            let agent = agent.clone();
            let activity = Arc::clone(&activity);
            let retry_settings = Arc::clone(&retry_settings);
            async move {
                let display = activity.display_text(&event);
                let committed = if let AgentEvent::MessageEnd { message } = &event {
                    Some(
                        activity
                            .message_ended(message)
                            .map_err(|error| EventError(error.to_string()))?,
                    )
                } else {
                    None
                };
                let Some(state) = agent.state() else {
                    return Ok(());
                };
                match project_product_user_event(event, display.as_deref()) {
                    AgentEvent::AgentEnd { messages } => {
                        let settings = *retry_settings
                            .read()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        let will_retry = will_retry_after_agent_end(
                            &messages,
                            settings,
                            activity.retry_attempt(),
                        );
                        events.publish_agent_end(messages, will_retry, state);
                    }
                    event => events.publish_agent(event, state),
                }
                if let Some(committed) = committed {
                    if let Some(queue) = committed.queue {
                        events.publish_queue(queue);
                    }
                    events.publish_entry(committed.entry);
                }
                Ok(())
            }
        },
    ));
}

fn project_product_user_event(event: AgentEvent, display_text: Option<&str>) -> AgentEvent {
    let Some(display_text) = display_text else {
        return event;
    };
    let display_message = |message: Message| match message {
        Message::User(user) => {
            let mut displayed = UserMessage::text(display_text, user.timestamp_ms);
            displayed.content.extend(
                user.content
                    .iter()
                    .filter(|block| matches!(block, ContentBlock::Image(_)))
                    .cloned(),
            );
            Message::User(displayed)
        }
        other => other,
    };
    match event {
        AgentEvent::MessageStart { message } => AgentEvent::MessageStart {
            message: display_message(message),
        },
        AgentEvent::MessageEnd { message } => AgentEvent::MessageEnd {
            message: display_message(message),
        },
        other => other,
    }
}
