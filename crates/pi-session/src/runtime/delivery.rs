//! Admission and timing for messages delivered through session capabilities.
use std::sync::Arc;

use pi_core::{ContentBlock, CustomMessage, CustomMessageContent, Message, UserMessage};

use super::activity::ActivityPhase;
use super::context::ContextActivation;
use super::{AgentSession, SessionInput};
use crate::{QueueKind, SessionEntry, SessionError};

pub(crate) enum SessionDelivery {
    Custom {
        message: CustomMessage,
        queue: Option<QueueKind>,
        trigger_turn: Option<bool>,
    },
    User {
        content: CustomMessageContent,
        queue: Option<QueueKind>,
        preprocess: bool,
    },
}

enum DeliveryPlan {
    Enqueue { message: Message, kind: QueueKind },
    Run(Message),
    Submit(SessionInput),
    Append { message: CustomMessage, wait: bool },
}

impl AgentSession {
    /// Immediate delivery stays synchronous; admitted background work pins this generation.
    pub(crate) fn dispatch_message(
        self: Arc<Self>,
        delivery: SessionDelivery,
        executor: &tokio::runtime::Handle,
    ) -> Result<(), SessionError> {
        self.ensure_open()?;
        match self.plan_delivery(delivery)? {
            DeliveryPlan::Enqueue { message, kind } => {
                self.enqueue_message(message, kind).map(|_| ())
            }
            DeliveryPlan::Append {
                message,
                wait: false,
            } => self.append_custom_message(message).map(|_| ()),
            plan => {
                executor.spawn(async move {
                    if let Err(error) = self.execute_delivery(plan).await {
                        self.notify_plugin(error.to_string(), crate::NoticeLevel::Error);
                    }
                });
                Ok(())
            }
        }
    }

    pub(crate) async fn deliver_message(
        &self,
        delivery: SessionDelivery,
    ) -> Result<(), SessionError> {
        self.ensure_open()?;
        self.execute_delivery(self.plan_delivery(delivery)?).await
    }

    fn plan_delivery(&self, delivery: SessionDelivery) -> Result<DeliveryPlan, SessionError> {
        let running = self.accepts_live_messages();
        Ok(match delivery {
            SessionDelivery::Custom {
                message,
                queue,
                trigger_turn,
            } => {
                if queue == Some(QueueKind::NextRun) {
                    DeliveryPlan::Enqueue {
                        message: Message::custom(message),
                        kind: QueueKind::NextRun,
                    }
                } else if running && trigger_turn != Some(false) {
                    DeliveryPlan::Enqueue {
                        message: Message::custom(message),
                        kind: queue.unwrap_or(QueueKind::Steer),
                    }
                } else if trigger_turn == Some(true) {
                    DeliveryPlan::Run(Message::custom(message))
                } else {
                    DeliveryPlan::Append {
                        message,
                        wait: !self.is_idle(),
                    }
                }
            }
            SessionDelivery::User {
                content,
                queue,
                preprocess,
            } => {
                if running && queue.is_none() {
                    return Err(SessionError::InvalidPayload(
                        "Agent is already processing. Specify deliverAs ('steer' or 'followUp') to queue the message.".into(),
                    ));
                }
                let (text, blocks) = user_content(content);
                let message = Message::User(UserMessage {
                    content: blocks.clone(),
                    timestamp_ms: crate::now_ms(),
                });
                if running {
                    DeliveryPlan::Enqueue {
                        message,
                        kind: queue.unwrap_or(QueueKind::Steer),
                    }
                } else if preprocess {
                    let images = blocks.into_iter().filter_map(|block| match block {
                        ContentBlock::Image(image) => Some(image),
                        _ => None,
                    });
                    DeliveryPlan::Submit(SessionInput::new(text).with_images(images))
                } else {
                    DeliveryPlan::Run(message)
                }
            }
        })
    }

    async fn execute_delivery(&self, plan: DeliveryPlan) -> Result<(), SessionError> {
        match plan {
            DeliveryPlan::Enqueue { message, kind } => {
                self.enqueue_message(message, kind).map(|_| ())
            }
            DeliveryPlan::Run(message) => self.run_messages(vec![message]).await.map(|_| ()),
            DeliveryPlan::Submit(input) => self.prompt(input).await.map(|_| ()),
            DeliveryPlan::Append {
                message,
                wait: false,
            } => self.append_custom_message(message).map(|_| ()),
            DeliveryPlan::Append {
                message,
                wait: true,
            } => {
                // Waiting and admission are one operation. A competing prompt cannot enter
                // between an idle notification and this context write.
                let _operation = self.activity.begin(ActivityPhase::Updating).await;
                self.ensure_open()?;
                self.context_updates.append(
                    SessionEntry::custom_message(&message),
                    ContextActivation::Restore,
                )?;
                Ok(())
            }
        }
    }
}

fn user_content(content: CustomMessageContent) -> (String, Vec<ContentBlock>) {
    match content {
        CustomMessageContent::Text(text) => {
            let blocks = vec![ContentBlock::Text(pi_core::TextContent::new(&text))];
            (text, blocks)
        }
        CustomMessageContent::Blocks(blocks) => {
            let text = blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            let mut normalized = vec![ContentBlock::Text(pi_core::TextContent::new(&text))];
            normalized.extend(
                blocks
                    .into_iter()
                    .filter(|block| matches!(block, ContentBlock::Image(_))),
            );
            (text, normalized)
        }
    }
}
