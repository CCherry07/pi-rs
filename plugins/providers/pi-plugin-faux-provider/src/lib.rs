#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream;
use pi_core::{
    AbortSignal, ModelId, PluginId, Provider, ProviderCallContext, ProviderError, ProviderId,
    ProviderPlugin, ProviderRegisterContext, ProviderRequest, ProviderStream, ResponseMetadata,
    StopReason, StreamEvent, ToolCall, Usage,
};

#[derive(Debug, Clone)]
pub enum FauxTurn {
    Text(String),
    ToolCalls(Vec<ToolCall>),
    Events(Vec<StreamEvent>),
    Error(String),
    WaitForAbort,
}

pub struct FauxProvider {
    id: ProviderId,
    model: ModelId,
    turns: tokio::sync::Mutex<VecDeque<FauxTurn>>,
    requests: Mutex<Vec<ProviderRequest>>,
}

impl FauxProvider {
    pub fn new(id: ProviderId, model: ModelId, turns: impl IntoIterator<Item = FauxTurn>) -> Self {
        Self {
            id,
            model,
            turns: tokio::sync::Mutex::new(turns.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    pub fn requests(&self) -> Vec<ProviderRequest> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn metadata(&self) -> ResponseMetadata {
        ResponseMetadata::new(self.id.clone(), self.model.clone(), "faux", now_ms())
    }
}

#[async_trait]
impl Provider for FauxProvider {
    fn id(&self) -> ProviderId {
        self.id.clone()
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        _context: ProviderCallContext,
        signal: AbortSignal,
    ) -> Result<ProviderStream, ProviderError> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(request);
        let turn = self
            .turns
            .lock()
            .await
            .pop_front()
            .ok_or_else(|| ProviderError::Protocol("no scripted turn remains".to_string()))?;
        let events = match turn {
            FauxTurn::Text(text) => vec![
                StreamEvent::Start {
                    metadata: self.metadata(),
                },
                StreamEvent::TextStart { content_index: 0 },
                StreamEvent::TextDelta {
                    content_index: 0,
                    delta: text,
                },
                StreamEvent::TextEnd {
                    content_index: 0,
                    text_signature: None,
                },
                StreamEvent::Done {
                    reason: StopReason::Stop,
                    usage: Usage::default(),
                },
            ],
            FauxTurn::ToolCalls(calls) => {
                let mut events = vec![StreamEvent::Start {
                    metadata: self.metadata(),
                }];
                for (content_index, call) in calls.into_iter().enumerate() {
                    events.push(StreamEvent::ToolCallStart {
                        content_index,
                        id: call.id,
                        name: call.name,
                    });
                    events.push(StreamEvent::ToolCallDelta {
                        content_index,
                        arguments_delta: call.arguments.to_string(),
                    });
                    events.push(StreamEvent::ToolCallEnd {
                        content_index,
                        thought_signature: call.thought_signature,
                    });
                }
                events.push(StreamEvent::Done {
                    reason: StopReason::ToolUse,
                    usage: Usage::default(),
                });
                events
            }
            FauxTurn::Events(events) => events,
            FauxTurn::Error(message) => return Err(ProviderError::Failure(message)),
            FauxTurn::WaitForAbort => {
                return Ok(Box::pin(stream::once(async move {
                    signal.wait().await;
                    Err(ProviderError::Aborted)
                })));
            }
        };
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }
}

pub struct FauxProviderPlugin {
    provider: Arc<FauxProvider>,
}

impl FauxProviderPlugin {
    pub fn scripted(turns: impl IntoIterator<Item = FauxTurn>) -> Self {
        Self {
            provider: Arc::new(FauxProvider::new(
                ProviderId::new("faux"),
                ModelId::new("test"),
                turns,
            )),
        }
    }

    pub fn provider(&self) -> Arc<FauxProvider> {
        Arc::clone(&self.provider)
    }
}

impl ProviderPlugin for FauxProviderPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("faux-provider")
    }

    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_core::Result<()> {
        context.register_provider(self.provider.clone())
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}
