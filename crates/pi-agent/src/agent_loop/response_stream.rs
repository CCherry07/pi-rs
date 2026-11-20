use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt;
use pi_core::{AbortSignal, AgentEvent, ModelCost, ResponseMetadataPatch, StreamEvent};
use pi_plugin::ProviderStream;

use crate::{AgentEventSink, StreamAssembler, StreamUpdate};

use super::AgentLoopError;

// Bound parser progress while a hook or frontend is awaiting delivery. The
// response itself remains in the single assembler; the queue holds only deltas.
const MAX_PENDING_UPDATES: usize = 32;

pub(super) struct ResponseStream {
    source: ProviderStream,
    pending: VecDeque<Result<StreamEvent, String>>,
    ended: bool,
    model_cost: Option<ModelCost>,
    request_started: Instant,
    pub assembler: StreamAssembler,
    pub chunk_count: u64,
    pub time_to_first_chunk_ms: Option<u64>,
}

impl ResponseStream {
    pub fn new(
        source: ProviderStream,
        model_cost: Option<ModelCost>,
        request_started: Instant,
    ) -> Self {
        Self {
            source,
            pending: VecDeque::new(),
            ended: false,
            model_cost,
            request_started,
            assembler: StreamAssembler::new(),
            chunk_count: 0,
            time_to_first_chunk_ms: None,
        }
    }

    pub async fn next(&mut self) -> Option<Result<StreamUpdate, String>> {
        let event = match self.pending.pop_front() {
            Some(event) => event,
            None => self.read().await?,
        };
        Some(self.assemble(event))
    }

    pub fn pop_pending(&mut self) -> Option<Result<StreamUpdate, String>> {
        let event = self.pending.pop_front()?;
        Some(self.assemble(event))
    }

    fn assemble(&mut self, event: Result<StreamEvent, String>) -> Result<StreamUpdate, String> {
        let event = event?;
        // Response metadata already reflects the latest source observation.
        // Replaying an older queued patch must not roll that observation back.
        if matches!(event, StreamEvent::Metadata { .. }) {
            return Ok(StreamUpdate {
                started: false,
                update: Some(Arc::new(event)),
            });
        }
        self.assembler
            .push(event)
            .map_err(|error| error.to_string())
    }

    async fn read(&mut self) -> Option<Result<StreamEvent, String>> {
        if self.ended {
            return None;
        }
        let Some(item) = self.source.next().await else {
            self.ended = true;
            return None;
        };
        self.chunk_count = self.chunk_count.saturating_add(1);
        self.time_to_first_chunk_ms.get_or_insert_with(|| {
            u64::try_from(self.request_started.elapsed().as_millis()).unwrap_or(u64::MAX)
        });
        let result = item
            .map_err(|error| error.to_string())
            .and_then(|mut event| {
                if let Some(cost) = &self.model_cost {
                    let usage = match &mut event {
                        StreamEvent::Done { usage, .. } => Some(usage),
                        StreamEvent::Metadata { patch } => patch.usage.as_mut(),
                        _ => None,
                    };
                    if let Some(usage) = usage
                        && usage.cost.total_from_provider != Some(true)
                    {
                        usage.cost = cost.calculate(usage);
                    }
                }
                // Content is assembled only when its delta is delivered, so
                // an incremental bridge can seed once and replay each delta.
                // Metadata is independently known as soon as the parser emits it.
                let observed = match &event {
                    StreamEvent::Metadata { patch } => Some(patch.clone()),
                    StreamEvent::Done { usage, .. } => Some(ResponseMetadataPatch {
                        usage: Some(usage.clone()),
                        ..Default::default()
                    }),
                    _ => None,
                };
                if let Some(patch) = observed {
                    self.assembler
                        .push(StreamEvent::Metadata { patch })
                        .map_err(|error| error.to_string())?;
                }
                Ok(event)
            });
        if result.is_err() {
            self.ended = true;
        }
        Some(result)
    }

    pub async fn emit(
        &mut self,
        events: &Arc<dyn AgentEventSink>,
        event: AgentEvent,
        signal: &AbortSignal,
    ) -> Result<(), AgentLoopError> {
        let delivery = super::emit(events, event, signal);
        tokio::pin!(delivery);
        let mut cancelled = signal.is_aborted();
        loop {
            tokio::select! {
                biased;
                () = signal.wait(), if !cancelled => cancelled = true,
                update = self.read(), if !cancelled && !self.ended && self.pending.len() < MAX_PENDING_UPDATES => {
                    if let Some(update) = update {
                        self.pending.push_back(update);
                    }
                }
                result = &mut delivery => return result,
            }
        }
    }
}
