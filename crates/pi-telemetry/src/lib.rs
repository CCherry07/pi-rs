#![forbid(unsafe_code)]

use std::future::Future;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParentKind {
    Any,
    RootOrExternal,
    Spans,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpanDefinition {
    pub name: &'static str,
    pub description: &'static str,
    pub parent_kind: ParentKind,
    pub parents: &'static [&'static str],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TelemetrySchema {
    pub version: u32,
    pub spans: &'static [SpanDefinition],
}

pub const AI_TELEMETRY_SCHEMA: TelemetrySchema = TelemetrySchema {
    version: 1,
    spans: &[SpanDefinition {
        name: "pi.ai.request",
        description: "One logical request to an AI provider",
        parent_kind: ParentKind::Any,
        parents: &[],
    }],
};

pub const HARNESS_TELEMETRY_SCHEMA: TelemetrySchema = TelemetrySchema {
    version: 1,
    spans: &[
        SpanDefinition {
            name: "pi.harness.run",
            description: "One admitted in-process run invocation",
            parent_kind: ParentKind::RootOrExternal,
            parents: &[],
        },
        SpanDefinition {
            name: "pi.harness.compaction",
            description: "One admitted in-process manual compaction invocation",
            parent_kind: ParentKind::RootOrExternal,
            parents: &[],
        },
        SpanDefinition {
            name: "pi.harness.navigation",
            description: "One admitted in-process navigation invocation",
            parent_kind: ParentKind::RootOrExternal,
            parents: &[],
        },
        SpanDefinition {
            name: "pi.harness.checkpoint",
            description: "One run checkpoint",
            parent_kind: ParentKind::Spans,
            parents: &["pi.harness.run"],
        },
        SpanDefinition {
            name: "pi.harness.turn",
            description: "One assistant response and its tool batch",
            parent_kind: ParentKind::Spans,
            parents: &["pi.harness.run"],
        },
        SpanDefinition {
            name: "pi.harness.step",
            description: "One durable retry attempt",
            parent_kind: ParentKind::Spans,
            parents: &[
                "pi.harness.turn",
                "pi.harness.checkpoint",
                "pi.harness.compaction",
                "pi.harness.navigation",
            ],
        },
        SpanDefinition {
            name: "pi.harness.tool",
            description: "One raw phase-2 tool execution",
            parent_kind: ParentKind::Spans,
            parents: &["pi.harness.turn", "pi.harness.run"],
        },
        SpanDefinition {
            name: "pi.harness.hook",
            description: "One registered hook handler invocation",
            parent_kind: ParentKind::Any,
            parents: &[],
        },
        SpanDefinition {
            name: "pi.harness.sleep",
            description: "One retry delay",
            parent_kind: ParentKind::Spans,
            parents: &["pi.harness.step", "pi.harness.run"],
        },
        SpanDefinition {
            name: "pi.harness.event_handler",
            description: "One passive event listener invocation",
            parent_kind: ParentKind::Any,
            parents: &[],
        },
        SpanDefinition {
            name: "pi.session.write",
            description: "One committed session mutation",
            parent_kind: ParentKind::Any,
            parents: &[],
        },
    ],
};

pub const AGENT_TELEMETRY_SCHEMAS: &[TelemetrySchema] =
    &[AI_TELEMETRY_SCHEMA, HARNESS_TELEMETRY_SCHEMA];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AiOperation {
    Stream,
    FetchDeferred,
    CancelDeferred,
    GenerateImages,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AiStopReason {
    Stop,
    Length,
    ToolUse,
    Error,
    Aborted,
    Deferred,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AiRequestStart {
    #[serde(rename = "pi.ai.operation")]
    pub operation: AiOperation,
    #[serde(rename = "pi.ai.provider")]
    pub provider: String,
    #[serde(rename = "pi.ai.model")]
    pub model: String,
    #[serde(rename = "pi.ai.api")]
    pub api: String,
    #[serde(rename = "pi.ai.streaming")]
    pub streaming: bool,
    #[serde(rename = "pi.ai.deferred", skip_serializing_if = "Option::is_none")]
    pub deferred: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct AiRequestEnd {
    #[serde(
        rename = "pi.ai.response.model",
        skip_serializing_if = "Option::is_none"
    )]
    pub response_model: Option<String>,
    #[serde(rename = "pi.ai.response.id", skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    #[serde(
        rename = "pi.ai.response.stop_reason",
        skip_serializing_if = "Option::is_none"
    )]
    pub stop_reason: Option<AiStopReason>,
    #[serde(
        rename = "pi.ai.http.status_code",
        skip_serializing_if = "Option::is_none"
    )]
    pub http_status_code: Option<u16>,
    #[serde(
        rename = "pi.ai.usage.input_tokens",
        skip_serializing_if = "Option::is_none"
    )]
    pub input_tokens: Option<u64>,
    #[serde(
        rename = "pi.ai.usage.output_tokens",
        skip_serializing_if = "Option::is_none"
    )]
    pub output_tokens: Option<u64>,
    #[serde(
        rename = "pi.ai.usage.cache_read_tokens",
        skip_serializing_if = "Option::is_none"
    )]
    pub cache_read_tokens: Option<u64>,
    #[serde(
        rename = "pi.ai.usage.cache_write_tokens",
        skip_serializing_if = "Option::is_none"
    )]
    pub cache_write_tokens: Option<u64>,
    #[serde(
        rename = "pi.ai.usage.reasoning_tokens",
        skip_serializing_if = "Option::is_none"
    )]
    pub reasoning_tokens: Option<u64>,
    #[serde(
        rename = "pi.ai.usage.total_tokens",
        skip_serializing_if = "Option::is_none"
    )]
    pub total_tokens: Option<u64>,
    #[serde(rename = "pi.ai.usage.cost", skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    #[serde(
        rename = "pi.ai.stream.chunk_count",
        skip_serializing_if = "Option::is_none"
    )]
    pub chunk_count: Option<u64>,
    #[serde(
        rename = "pi.ai.stream.time_to_first_chunk_ms",
        skip_serializing_if = "Option::is_none"
    )]
    pub time_to_first_chunk_ms: Option<u64>,
    #[serde(rename = "pi.ai.error.type", skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OperationStartAttributes {
    #[serde(rename = "pi.session.id")]
    pub session_id: String,
    #[serde(rename = "pi.lane.name")]
    pub lane_name: String,
    #[serde(rename = "pi.operation.id")]
    pub operation_id: String,
    #[serde(rename = "pi.operation.recovery")]
    pub recovery: bool,
}

macro_rules! string_enum {
    ($name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
        pub enum $name {
            $(#[serde(rename = $value)] $variant),+
        }
    };
}

string_enum!(RunOperationKind { Run => "run" });
string_enum!(CompactionOperationKind { Compaction => "compaction" });
string_enum!(NavigationOperationKind { Navigation => "navigation" });
string_enum!(RunOutcome {
    Completed => "completed",
    Aborted => "aborted",
    Failed => "failed",
    Suspended => "suspended"
});
string_enum!(OperationOutcome {
    Completed => "completed",
    Declined => "declined",
    Aborted => "aborted",
    Failed => "failed"
});
string_enum!(CheckpointKind {
    Normal => "normal",
    FailureDrain => "failure_drain",
    AbortReconcile => "abort_reconcile"
});
string_enum!(StepKind {
    Assistant => "assistant",
    Compaction => "compaction",
    BranchSummary => "branch_summary"
});
string_enum!(CompactionReason {
    Manual => "manual",
    Threshold => "threshold",
    Overflow => "overflow"
});
string_enum!(StepOutcome {
    Succeeded => "succeeded",
    Retry => "retry",
    Failed => "failed",
    Aborted => "aborted",
    Deferred => "deferred",
    Overflow => "overflow"
});
string_enum!(ToolReplay { Never => "never", Safe => "safe" });
string_enum!(HookOutcome {
    Completed => "completed",
    Skipped => "skipped",
    Blocked => "blocked",
    Failed => "failed"
});
string_enum!(SleepOutcome { Elapsed => "elapsed", Aborted => "aborted" });
string_enum!(SessionMutation {
    Entry => "entry",
    Record => "record",
    Lane => "lane",
    Fact => "fact"
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunStart {
    #[serde(flatten)]
    pub operation: OperationStartAttributes,
    #[serde(rename = "pi.operation.kind")]
    pub kind: RunOperationKind,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RunEnd {
    #[serde(
        rename = "pi.operation.outcome",
        skip_serializing_if = "Option::is_none"
    )]
    pub outcome: Option<RunOutcome>,
    #[serde(rename = "pi.error.code", skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(rename = "pi.error.type", skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompactionStart {
    #[serde(flatten)]
    pub operation: OperationStartAttributes,
    #[serde(rename = "pi.operation.kind")]
    pub kind: CompactionOperationKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NavigationStart {
    #[serde(flatten)]
    pub operation: OperationStartAttributes,
    #[serde(rename = "pi.operation.kind")]
    pub kind: NavigationOperationKind,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct OperationEnd {
    #[serde(
        rename = "pi.operation.outcome",
        skip_serializing_if = "Option::is_none"
    )]
    pub outcome: Option<OperationOutcome>,
    #[serde(rename = "pi.error.code", skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(rename = "pi.error.type", skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckpointStart {
    #[serde(rename = "pi.lane.name")]
    pub lane_name: String,
    #[serde(rename = "pi.operation.id")]
    pub operation_id: String,
    #[serde(rename = "pi.checkpoint.kind")]
    pub kind: CheckpointKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TurnStart {
    #[serde(rename = "pi.lane.name")]
    pub lane_name: String,
    #[serde(rename = "pi.operation.id")]
    pub operation_id: String,
    #[serde(rename = "pi.turn.id")]
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StepStart {
    #[serde(rename = "pi.lane.name")]
    pub lane_name: String,
    #[serde(rename = "pi.operation.id")]
    pub operation_id: String,
    #[serde(rename = "pi.step.kind")]
    pub kind: StepKind,
    #[serde(rename = "pi.step.attempt")]
    pub attempt: u32,
    #[serde(
        rename = "pi.compaction.reason",
        skip_serializing_if = "Option::is_none"
    )]
    pub compaction_reason: Option<CompactionReason>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct StepEnd {
    #[serde(rename = "pi.step.outcome", skip_serializing_if = "Option::is_none")]
    pub outcome: Option<StepOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolStart {
    #[serde(rename = "pi.lane.name")]
    pub lane_name: String,
    #[serde(rename = "pi.operation.id")]
    pub operation_id: String,
    #[serde(rename = "pi.turn.id", skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(rename = "pi.tool.name")]
    pub tool_name: String,
    #[serde(rename = "pi.tool.call_id")]
    pub tool_call_id: String,
    #[serde(rename = "pi.tool.replay")]
    pub replay: ToolReplay,
    #[serde(rename = "pi.tool.recovery")]
    pub recovery: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ToolEnd {
    #[serde(rename = "pi.tool.is_error", skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

string_enum!(HookName {
    BeforeRun => "before_run",
    BeforeResume => "before_resume",
    BeforeRunEnd => "before_run_end",
    TransformContext => "transform_context",
    BeforeRequest => "before_request",
    BeforePayload => "before_payload",
    AfterResponse => "after_response",
    BeforeTool => "before_tool",
    AfterTool => "after_tool",
    BeforeCompaction => "before_compaction",
    BeforeNavigation => "before_navigation"
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HookStart {
    #[serde(rename = "pi.lane.name")]
    pub lane_name: String,
    #[serde(rename = "pi.operation.id", skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(rename = "pi.hook.name")]
    pub name: HookName,
    #[serde(
        rename = "pi.hook.registration_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub registration_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct HookEnd {
    #[serde(rename = "pi.hook.outcome", skip_serializing_if = "Option::is_none")]
    pub outcome: Option<HookOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SleepStart {
    #[serde(rename = "pi.operation.id")]
    pub operation_id: String,
    #[serde(rename = "pi.sleep.delay_ms")]
    pub delay_ms: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SleepEnd {
    #[serde(rename = "pi.sleep.outcome", skip_serializing_if = "Option::is_none")]
    pub outcome: Option<SleepOutcome>,
}

string_enum!(HarnessEventType {
    RunStart => "run_start",
    RunResume => "run_resume",
    RunSuspend => "run_suspend",
    RunAbort => "run_abort",
    RunEnd => "run_end",
    Fault => "fault",
    HandlerError => "handler_error",
    TurnStart => "turn_start",
    TurnEnd => "turn_end",
    RetryScheduled => "retry_scheduled",
    RetryStart => "retry_start",
    RetryEnd => "retry_end",
    MessageStart => "message_start",
    MessageUpdate => "message_update",
    MessageEnd => "message_end",
    ToolStart => "tool_start",
    ToolUpdate => "tool_update",
    ToolEnd => "tool_end",
    EntryAdded => "entry_added",
    WritePending => "write_pending",
    QueueUpdate => "queue_update",
    FactUpdate => "fact_update",
    ConfigUpdate => "config_update",
    CompactionStart => "compaction_start",
    CompactionEnd => "compaction_end",
    NavigationStart => "navigation_start",
    NavigationEnd => "navigation_end",
    LaneCreated => "lane_created",
    Usage => "usage"
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EventHandlerStart {
    #[serde(rename = "pi.event.type")]
    pub event_type: HarnessEventType,
    #[serde(rename = "pi.lane.name", skip_serializing_if = "Option::is_none")]
    pub lane_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionWriteStart {
    #[serde(rename = "pi.lane.name")]
    pub lane_name: String,
    #[serde(rename = "pi.operation.id", skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(rename = "pi.session.mutation")]
    pub mutation: SessionMutation,
    #[serde(
        rename = "pi.session.item_type",
        skip_serializing_if = "Option::is_none"
    )]
    pub item_type: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SessionWriteEnd {
    #[serde(rename = "pi.session.seq", skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct EmptyEnd {}

mod sealed {
    pub trait Sealed {}
}

pub trait SpanSpec: sealed::Sealed + Send + Sync + 'static {
    type Start: Serialize + Send + Sync + 'static;
    type End: Default + Serialize + Send + Sync + 'static;
    const NAME: &'static str;
}

macro_rules! define_span {
    ($marker:ident, $start:ty, $end:ty, $name:literal) => {
        pub struct $marker;
        impl sealed::Sealed for $marker {}
        impl SpanSpec for $marker {
            type Start = $start;
            type End = $end;
            const NAME: &'static str = $name;
        }
    };
}

define_span!(AiRequestSpan, AiRequestStart, AiRequestEnd, "pi.ai.request");
define_span!(RunSpan, RunStart, RunEnd, "pi.harness.run");
define_span!(
    CompactionSpan,
    CompactionStart,
    OperationEnd,
    "pi.harness.compaction"
);
define_span!(
    NavigationSpan,
    NavigationStart,
    OperationEnd,
    "pi.harness.navigation"
);
define_span!(
    CheckpointSpan,
    CheckpointStart,
    EmptyEnd,
    "pi.harness.checkpoint"
);
define_span!(TurnSpan, TurnStart, EmptyEnd, "pi.harness.turn");
define_span!(StepSpan, StepStart, StepEnd, "pi.harness.step");
define_span!(ToolSpan, ToolStart, ToolEnd, "pi.harness.tool");
define_span!(HookSpan, HookStart, HookEnd, "pi.harness.hook");
define_span!(SleepSpan, SleepStart, SleepEnd, "pi.harness.sleep");
define_span!(
    EventHandlerSpan,
    EventHandlerStart,
    EmptyEnd,
    "pi.harness.event_handler"
);
define_span!(
    SessionWriteSpan,
    SessionWriteStart,
    SessionWriteEnd,
    "pi.session.write"
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanStatus {
    Ok,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum TelemetryRecord {
    Start {
        id: u64,
        parent_id: Option<u64>,
        name: String,
        attributes: Value,
    },
    End {
        id: u64,
        name: String,
        attributes: Value,
        status: SpanStatus,
    },
}

pub trait TelemetrySink: Send + Sync {
    fn record(&self, record: TelemetryRecord);
}

struct NoopTelemetrySink;

impl TelemetrySink for NoopTelemetrySink {
    fn record(&self, _record: TelemetryRecord) {}
}

#[derive(Default)]
pub struct InMemoryTelemetrySink {
    records: Mutex<Vec<TelemetryRecord>>,
}

impl InMemoryTelemetrySink {
    pub fn records(&self) -> Vec<TelemetryRecord> {
        self.records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl TelemetrySink for InMemoryTelemetrySink {
    fn record(&self, record: TelemetryRecord) {
        self.records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(record);
    }
}

#[derive(Clone)]
pub struct TelemetryContext {
    sink: Arc<dyn TelemetrySink>,
    next_id: Arc<AtomicU64>,
    parent_id: Option<u64>,
}

impl Default for TelemetryContext {
    fn default() -> Self {
        Self::noop()
    }
}

impl TelemetryContext {
    pub fn noop() -> Self {
        Self::new(Arc::new(NoopTelemetrySink))
    }

    pub fn new(sink: Arc<dyn TelemetrySink>) -> Self {
        Self {
            sink,
            next_id: Arc::new(AtomicU64::new(1)),
            parent_id: None,
        }
    }

    pub fn start_span<K: SpanSpec>(&self, start: K::Start) -> ActiveSpan<K> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.sink.record(TelemetryRecord::Start {
            id,
            parent_id: self.parent_id,
            name: K::NAME.to_string(),
            attributes: serde_json::to_value(start).expect("typed telemetry attributes serialize"),
        });
        ActiveSpan {
            id,
            sink: Arc::clone(&self.sink),
            next_id: Arc::clone(&self.next_id),
            end: Mutex::new(K::End::default()),
            status: Mutex::new(SpanStatus::Ok),
            finished: AtomicBool::new(false),
            marker: PhantomData,
        }
    }

    pub async fn in_span<K, F, Fut, T>(&self, start: K::Start, callback: F) -> T
    where
        K: SpanSpec,
        F: FnOnce(&ActiveSpan<K>) -> Fut,
        Fut: Future<Output = T>,
    {
        let span = self.start_span::<K>(start);
        callback(&span).await
    }
}

pub struct ActiveSpan<K: SpanSpec> {
    id: u64,
    sink: Arc<dyn TelemetrySink>,
    next_id: Arc<AtomicU64>,
    end: Mutex<K::End>,
    status: Mutex<SpanStatus>,
    finished: AtomicBool,
    marker: PhantomData<K>,
}

impl<K: SpanSpec> ActiveSpan<K> {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn set_end_attributes(&self, attributes: K::End) {
        *self
            .end
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = attributes;
    }

    pub fn set_status(&self, status: SpanStatus) {
        *self
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = status;
    }

    pub fn child_context(&self) -> TelemetryContext {
        TelemetryContext {
            sink: Arc::clone(&self.sink),
            next_id: Arc::clone(&self.next_id),
            parent_id: Some(self.id),
        }
    }

    pub fn finish(&self) {
        if self.finished.swap(true, Ordering::AcqRel) {
            return;
        }
        let attributes = {
            let attributes = self
                .end
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            serde_json::to_value(&*attributes).expect("typed telemetry attributes serialize")
        };
        let status = *self
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.sink.record(TelemetryRecord::End {
            id: self.id,
            name: K::NAME.to_string(),
            attributes,
            status,
        });
    }
}

impl<K: SpanSpec> Drop for ActiveSpan<K> {
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn operation() -> OperationStartAttributes {
        OperationStartAttributes {
            session_id: "session".to_string(),
            lane_name: "main".to_string(),
            operation_id: "operation".to_string(),
            recovery: false,
        }
    }

    #[test]
    fn schemas_serialize_and_preserve_the_pi_span_vocabulary() {
        serde_json::to_string(AGENT_TELEMETRY_SCHEMAS).unwrap();
        assert_eq!(
            HARNESS_TELEMETRY_SCHEMA
                .spans
                .iter()
                .map(|span| span.name)
                .collect::<Vec<_>>(),
            vec![
                "pi.harness.run",
                "pi.harness.compaction",
                "pi.harness.navigation",
                "pi.harness.checkpoint",
                "pi.harness.turn",
                "pi.harness.step",
                "pi.harness.tool",
                "pi.harness.hook",
                "pi.harness.sleep",
                "pi.harness.event_handler",
                "pi.session.write",
            ]
        );
    }

    #[tokio::test]
    async fn typed_spans_record_nested_start_and_end_lifecycle() {
        let sink = Arc::new(InMemoryTelemetrySink::default());
        let context = TelemetryContext::new(sink.clone());

        context
            .in_span::<RunSpan, _, _, _>(
                RunStart {
                    operation: operation(),
                    kind: RunOperationKind::Run,
                },
                |run| {
                    run.set_end_attributes(RunEnd {
                        outcome: Some(RunOutcome::Completed),
                        ..RunEnd::default()
                    });
                    let child = run.child_context();
                    async move {
                        child
                            .in_span::<StepSpan, _, _, _>(
                                StepStart {
                                    lane_name: "main".to_string(),
                                    operation_id: "operation".to_string(),
                                    kind: StepKind::Assistant,
                                    attempt: 1,
                                    compaction_reason: None,
                                },
                                |step| {
                                    step.set_end_attributes(StepEnd {
                                        outcome: Some(StepOutcome::Succeeded),
                                    });
                                    let ai = step.child_context();
                                    async move {
                                        ai.in_span::<AiRequestSpan, _, _, _>(
                                            AiRequestStart {
                                                operation: AiOperation::Stream,
                                                provider: "provider".to_string(),
                                                model: "model".to_string(),
                                                api: "api".to_string(),
                                                streaming: true,
                                                deferred: None,
                                            },
                                            |request| {
                                                request.set_end_attributes(AiRequestEnd {
                                                    stop_reason: Some(AiStopReason::Stop),
                                                    ..AiRequestEnd::default()
                                                });
                                                async move {}
                                            },
                                        )
                                        .await;
                                    }
                                },
                            )
                            .await;
                    }
                },
            )
            .await;

        let records = sink.records();
        assert_eq!(records.len(), 6);
        assert!(matches!(
            &records[0],
            TelemetryRecord::Start { name, parent_id: None, .. } if name == "pi.harness.run"
        ));
        assert!(matches!(
            &records[1],
            TelemetryRecord::Start { parent_id: Some(1), name, .. } if name == "pi.harness.step"
        ));
        assert!(matches!(
            &records[2],
            TelemetryRecord::Start { parent_id: Some(2), name, attributes, .. }
                if name == "pi.ai.request" && attributes["pi.ai.operation"] == "stream"
        ));
        assert!(matches!(
            &records[5],
            TelemetryRecord::End { name, attributes, status: SpanStatus::Ok, .. }
                if name == "pi.harness.run" && attributes["pi.operation.outcome"] == "completed"
        ));
    }

    #[tokio::test]
    async fn noop_context_executes_callbacks_without_recording_requirements() {
        let value = TelemetryContext::noop()
            .in_span::<CheckpointSpan, _, _, _>(
                CheckpointStart {
                    lane_name: "main".to_string(),
                    operation_id: "operation".to_string(),
                    kind: CheckpointKind::Normal,
                },
                |_| async { 42 },
            )
            .await;
        assert_eq!(value, 42);
    }
}
