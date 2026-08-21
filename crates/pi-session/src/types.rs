use std::collections::HashMap;
use std::path::PathBuf;

use pi_core::{Message, ModelId, ProviderId, Usage, UsageCost};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

pub const SESSION_SCHEMA_VERSION: u32 = 4;
pub const MAIN_LANE: &str = "main";

/// Pi's extensible agent-level message. Standard provider messages retain
/// their typed representation; extension-defined roles are preserved as JSON
/// so a Rust session can round-trip TypeScript plugin messages losslessly.
#[derive(Debug, Clone)]
pub struct AgentMessage(AgentMessageKind);

#[derive(Debug, Clone)]
enum AgentMessageKind {
    Standard {
        message: Message,
        /// Keep the decoded wire object so extension fields and the string
        /// form of user content survive a read/write cycle.
        original: Option<Value>,
    },
    Custom {
        role: String,
        value: Value,
    },
}

impl AgentMessage {
    pub fn custom(value: Value) -> Result<Self, SessionError> {
        let role = value
            .as_object()
            .and_then(|object| object.get("role"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                SessionError::InvalidPayload(
                    "agent custom message must be an object with a string role".to_string(),
                )
            })?
            .to_string();
        if matches!(role.as_str(), "user" | "assistant" | "toolResult") {
            let message = serde_json::from_value(value.clone())
                .map_err(|error| SessionError::InvalidPayload(error.to_string()))?;
            Ok(Self(AgentMessageKind::Standard {
                message,
                original: Some(value),
            }))
        } else {
            Ok(Self(AgentMessageKind::Custom { role, value }))
        }
    }

    pub fn role(&self) -> &str {
        match &self.0 {
            AgentMessageKind::Standard {
                message: Message::User(_),
                ..
            } => "user",
            AgentMessageKind::Standard {
                message: Message::Assistant(_),
                ..
            } => "assistant",
            AgentMessageKind::Standard {
                message: Message::ToolResult(_),
                ..
            } => "toolResult",
            AgentMessageKind::Custom { role, .. } => role,
        }
    }

    pub fn as_standard(&self) -> Option<&Message> {
        match &self.0 {
            AgentMessageKind::Standard { message, .. } => Some(message),
            AgentMessageKind::Custom { .. } => None,
        }
    }

    pub fn as_custom(&self) -> Option<&Value> {
        match &self.0 {
            AgentMessageKind::Standard { .. } => None,
            AgentMessageKind::Custom { value, .. } => Some(value),
        }
    }
}

impl PartialEq for AgentMessage {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (
                AgentMessageKind::Standard { message: left, .. },
                AgentMessageKind::Standard { message: right, .. },
            ) => left == right,
            (
                AgentMessageKind::Custom { value: left, .. },
                AgentMessageKind::Custom { value: right, .. },
            ) => left == right,
            _ => false,
        }
    }
}

impl From<Message> for AgentMessage {
    fn from(message: Message) -> Self {
        Self(AgentMessageKind::Standard {
            message,
            original: None,
        })
    }
}

impl Serialize for AgentMessage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match &self.0 {
            AgentMessageKind::Standard { message, original } => match original {
                Some(value) => value.serialize(serializer),
                None => message.serialize(serializer),
            },
            AgentMessageKind::Custom { value, .. } => value.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for AgentMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        Self::custom(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("session I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid session JSON at line {line}: {message}")]
    InvalidJson { line: usize, message: String },
    #[error("session header is missing or is not the first line")]
    MissingHeader,
    #[error("unsupported session schema version: {0}")]
    UnsupportedSchema(u32),
    #[error("session id already exists: {0}")]
    AlreadyExists(String),
    #[error("session entry was not found: {0}")]
    NotFound(String),
    #[error("invalid session entry: {0}")]
    InvalidEntry(String),
    #[error("invalid durable payload: {0}")]
    InvalidPayload(String),
    #[error("invalid lane: {0}")]
    InvalidLane(String),
    #[error("invalid session query: {0}")]
    InvalidQuery(String),
    #[error("invalid fork target: {0}")]
    InvalidForkTarget(String),
    #[error("session storage failed: {0}")]
    Storage(String),
    #[error("runtime operation failed: {0}")]
    Runtime(String),
    #[error(transparent)]
    SessionPlugin(#[from] crate::SessionPluginError),
    #[error("session is busy with another operation")]
    Busy,
    #[error("session is closed")]
    Closed,
    #[error("{0} cancelled")]
    Cancelled(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionErrorCode {
    NotFound,
    AlreadyExists,
    InvalidEntry,
    InvalidPayload,
    InvalidLane,
    InvalidQuery,
    InvalidForkTarget,
    Storage,
}

impl SessionError {
    /// Stable category matching Pi's `SessionErrorCode` contract.
    pub fn code(&self) -> SessionErrorCode {
        match self {
            Self::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
                SessionErrorCode::NotFound
            }
            Self::NotFound(_) => SessionErrorCode::NotFound,
            Self::AlreadyExists(_) => SessionErrorCode::AlreadyExists,
            Self::InvalidJson { .. }
            | Self::MissingHeader
            | Self::UnsupportedSchema(_)
            | Self::InvalidEntry(_) => SessionErrorCode::InvalidEntry,
            Self::InvalidPayload(_) => SessionErrorCode::InvalidPayload,
            Self::InvalidLane(_) => SessionErrorCode::InvalidLane,
            Self::InvalidQuery(_) => SessionErrorCode::InvalidQuery,
            Self::InvalidForkTarget(_) => SessionErrorCode::InvalidForkTarget,
            Self::Io(_)
            | Self::Storage(_)
            | Self::Runtime(_)
            | Self::SessionPlugin(_)
            | Self::Busy
            | Self::Closed
            | Self::Cancelled(_) => SessionErrorCode::Storage,
        }
    }
}

impl From<pi_runtime::RuntimeError> for SessionError {
    fn from(error: pi_runtime::RuntimeError) -> Self {
        Self::Runtime(error.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HeaderKind {
    Header,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHeader {
    pub kind: HeaderKind,
    pub version: u32,
    pub id: String,
    pub created_at: i64,
    pub cwd: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub legacy_parent_session_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
}

impl SessionHeader {
    pub fn new(id: impl Into<String>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            kind: HeaderKind::Header,
            version: SESSION_SCHEMA_VERSION,
            id: id.into(),
            created_at: crate::now_ms(),
            cwd: cwd.into(),
            parent_session_id: None,
            legacy_parent_session_path: None,
            metadata: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageEntry {
    pub message: AgentMessage,
    #[serde(default, skip_serializing_if = "is_false")]
    pub terminate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelChangeEntry {
    pub provider: ProviderId,
    pub model_id: ModelId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingLevelEntry {
    pub thinking_level: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveToolsEntry {
    pub active_tool_names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionEntry {
    pub summary: String,
    #[serde(default)]
    pub retained_tail: Vec<AgentMessage>,
    pub tokens_before: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryEntry {
    pub from_id: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomEntry {
    pub custom_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEntry {
    Message(MessageEntry),
    ModelChange(ModelChangeEntry),
    ThinkingLevelChange(ThinkingLevelEntry),
    ActiveToolsChange(ActiveToolsEntry),
    Compaction(CompactionEntry),
    BranchSummary(BranchSummaryEntry),
    Custom(CustomEntry),
}

impl SessionEntry {
    pub fn message(message: impl Into<AgentMessage>) -> Self {
        Self::Message(MessageEntry {
            message: message.into(),
            terminate: false,
        })
    }

    pub fn entry_type(&self) -> SessionEntryType {
        match self {
            Self::Message(_) => SessionEntryType::Message,
            Self::ModelChange(_) => SessionEntryType::ModelChange,
            Self::ThinkingLevelChange(_) => SessionEntryType::ThinkingLevelChange,
            Self::ActiveToolsChange(_) => SessionEntryType::ActiveToolsChange,
            Self::Compaction(_) => SessionEntryType::Compaction,
            Self::BranchSummary(_) => SessionEntryType::BranchSummary,
            Self::Custom(_) => SessionEntryType::Custom,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEntryType {
    Message,
    ModelChange,
    ThinkingLevelChange,
    ActiveToolsChange,
    Compaction,
    BranchSummary,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionedEntry {
    pub id: String,
    #[serde(flatten)]
    pub entry: SessionEntry,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRecord {
    pub id: String,
    pub seq: u64,
    pub parent_id: Option<String>,
    #[serde(rename = "timestamp")]
    pub timestamp_ms: i64,
    #[serde(flatten)]
    pub entry: SessionEntry,
}

impl SessionRecord {
    pub fn provisioned(&self) -> ProvisionedEntry {
        ProvisionedEntry {
            id: self.id.clone(),
            entry: self.entry.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Run,
    Compaction,
    Navigation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum OperationIntent {
    Run {
        original_prompt: Vec<AgentMessage>,
        initial_messages: Vec<ProvisionedEntry>,
        #[serde(skip_serializing_if = "Option::is_none")]
        system_prompt_override: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        resume_data: Option<HashMap<String, Value>>,
    },
    Compaction {
        #[serde(skip_serializing_if = "Option::is_none")]
        custom_instructions: Option<String>,
        result_entry_id: String,
    },
    Navigation {
        target_id: Option<String>,
        summarize: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        custom_instructions: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        summary_entry_id: Option<String>,
    },
}

impl OperationIntent {
    pub fn kind(&self) -> OperationKind {
        match self {
            Self::Run { .. } => OperationKind::Run,
            Self::Compaction { .. } => OperationKind::Compaction,
            Self::Navigation { .. } => OperationKind::Navigation,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationOutcome {
    Completed,
    Aborted,
    Failed,
    Declined,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    Assistant,
    Compaction,
    BranchSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompactionReason {
    Manual,
    Threshold,
    Overflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum QueueKind {
    Steer,
    FollowUp,
    NextRun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolReplay {
    Never,
    Safe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionStopReason {
    Stop,
    Length,
    ToolUse,
    Error,
    Aborted,
    Deferred,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUsage {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_1h: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<i64>,
    pub total_tokens: i64,
    #[serde(default)]
    pub cost: UsageCost,
}

impl From<&Usage> for SessionUsage {
    fn from(usage: &Usage) -> Self {
        Self {
            input: saturating_i64(usage.input),
            output: saturating_i64(usage.output),
            cache_read: saturating_i64(usage.cache_read),
            cache_write: saturating_i64(usage.cache_write),
            cache_write_1h: usage.cache_write_1h.map(saturating_i64),
            reasoning: usage.reasoning.map(saturating_i64),
            total_tokens: saturating_i64(usage.total_tokens),
            cost: usage.cost.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "cause",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum UsageAttribution {
    Assistant {
        run_id: String,
        entry_id: String,
        attempt: u32,
        stop_reason: SessionStopReason,
    },
    Compaction {
        run_id: String,
        entry_id: String,
        attempt: u32,
        stop_reason: SessionStopReason,
    },
    BranchSummary {
        run_id: String,
        entry_id: String,
        attempt: u32,
        stop_reason: SessionStopReason,
    },
    DeferredFetch {
        run_id: String,
        entry_id: String,
        attempt: u32,
        stop_reason: SessionStopReason,
    },
    Tool {
        run_id: String,
        entry_id: String,
        tool_call_id: String,
    },
    Hook {
        run_id: String,
        entry_id: String,
    },
    Adjustment {
        #[serde(skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        entry_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        details: Option<Value>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecord {
    pub usage: SessionUsage,
    #[serde(flatten)]
    pub attribution: UsageAttribution,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum LaneRecordEntry {
    OperationStarted {
        source_leaf_id: Option<String>,
        intent: OperationIntent,
    },
    AbortRequested {
        run_id: String,
    },
    OperationFinished {
        run_id: String,
        outcome: OperationOutcome,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<OperationError>,
    },
    StepAttempt {
        run_id: String,
        step: StepKind,
        attempt: u32,
        result_entry_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        compaction_reason: Option<CompactionReason>,
    },
    ToolStarted {
        run_id: String,
        assistant_entry_id: String,
        tool_index: usize,
        tool_call_id: String,
        tool_name: String,
        effective_args: Map<String, Value>,
        result_entry_id: String,
        replay: ToolReplay,
    },
    QueueEnqueued {
        queue: QueueKind,
        #[serde(skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
        target: ProvisionedEntry,
    },
    QueueCancelled {
        #[serde(skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
        entry_id: String,
    },
    WriteDeferred {
        run_id: String,
        target: ProvisionedEntry,
    },
    Usage(UsageRecord),
}

impl LaneRecordEntry {
    pub fn record_type(&self) -> LaneRecordType {
        match self {
            Self::OperationStarted { .. } => LaneRecordType::OperationStarted,
            Self::AbortRequested { .. } => LaneRecordType::AbortRequested,
            Self::OperationFinished { .. } => LaneRecordType::OperationFinished,
            Self::StepAttempt { .. } => LaneRecordType::StepAttempt,
            Self::ToolStarted { .. } => LaneRecordType::ToolStarted,
            Self::QueueEnqueued { .. } => LaneRecordType::QueueEnqueued,
            Self::QueueCancelled { .. } => LaneRecordType::QueueCancelled,
            Self::WriteDeferred { .. } => LaneRecordType::WriteDeferred,
            Self::Usage(_) => LaneRecordType::Usage,
        }
    }

    pub fn run_id(&self) -> Option<&str> {
        match self {
            Self::OperationStarted { .. } => None,
            Self::AbortRequested { run_id }
            | Self::OperationFinished { run_id, .. }
            | Self::StepAttempt { run_id, .. }
            | Self::ToolStarted { run_id, .. }
            | Self::WriteDeferred { run_id, .. } => Some(run_id),
            Self::QueueEnqueued { run_id, .. } | Self::QueueCancelled { run_id, .. } => {
                run_id.as_deref()
            }
            Self::Usage(record) => match &record.attribution {
                UsageAttribution::Assistant { run_id, .. }
                | UsageAttribution::Compaction { run_id, .. }
                | UsageAttribution::BranchSummary { run_id, .. }
                | UsageAttribution::DeferredFetch { run_id, .. }
                | UsageAttribution::Tool { run_id, .. }
                | UsageAttribution::Hook { run_id, .. } => Some(run_id),
                UsageAttribution::Adjustment { run_id, .. } => run_id.as_deref(),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneRecordType {
    OperationStarted,
    AbortRequested,
    OperationFinished,
    StepAttempt,
    ToolStarted,
    QueueEnqueued,
    QueueCancelled,
    WriteDeferred,
    Usage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewLaneRecord {
    pub id: String,
    pub lane: String,
    #[serde(flatten)]
    pub record: LaneRecordEntry,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaneRecord {
    pub id: String,
    pub seq: u64,
    pub lane: String,
    #[serde(rename = "timestamp")]
    pub timestamp_ms: i64,
    #[serde(flatten)]
    pub record: LaneRecordEntry,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LanePointer {
    pub lane: String,
    pub leaf_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "fact",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum SessionFact {
    Name {
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Label {
        target_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum SessionMutation {
    Entry {
        #[serde(skip_serializing_if = "Option::is_none")]
        lane: Option<String>,
        #[serde(flatten)]
        record: SessionRecord,
    },
    Record {
        #[serde(flatten)]
        record: LaneRecord,
    },
    Lane {
        seq: u64,
        lane: String,
        leaf_id: Option<String>,
    },
    Fact {
        seq: u64,
        #[serde(flatten)]
        fact: SessionFact,
    },
}

impl SessionMutation {
    pub fn seq(&self) -> u64 {
        match self {
            Self::Entry { record, .. } => record.seq,
            Self::Record { record } => record.seq,
            Self::Lane { seq, .. } | Self::Fact { seq, .. } => *seq,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum LogItem {
    Entry {
        seq: u64,
        entry: SessionRecord,
    },
    Record {
        seq: u64,
        record: LaneRecord,
    },
    Lane {
        seq: u64,
        lane: String,
        leaf_id: Option<String>,
    },
    Fact {
        seq: u64,
        #[serde(flatten)]
        fact: SessionFact,
    },
}

impl LogItem {
    pub fn seq(&self) -> u64 {
        match self {
            Self::Entry { seq, .. }
            | Self::Record { seq, .. }
            | Self::Lane { seq, .. }
            | Self::Fact { seq, .. } => *seq,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EntryOrder {
    OldestFirst,
    #[default]
    NewestFirst,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntryQuery {
    pub entry_type: Option<SessionEntryType>,
    pub custom_type: Option<String>,
    pub order: EntryOrder,
    pub limit: Option<usize>,
    pub after_seq: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BranchQuery {
    pub start: Option<String>,
    pub stop_at_type: Option<SessionEntryType>,
    pub stop_at_id: Option<String>,
    pub entries: EntryQuery,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecordQuery {
    pub lane: Option<String>,
    pub record_type: Option<LaneRecordType>,
    pub run_id: Option<String>,
    pub operation_kind: Option<OperationKind>,
    pub after_seq: Option<u64>,
    pub order: EntryOrder,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionStats {
    pub message_count: u64,
    pub cached_tokens: i64,
    pub uncached_tokens: i64,
    pub total_tokens: i64,
    pub cost_total: f64,
}

#[derive(Debug, Clone)]
pub struct SessionDocument {
    pub header: SessionHeader,
    pub entries: Vec<SessionRecord>,
    pub records: Vec<LaneRecord>,
    pub lanes: Vec<LanePointer>,
    pub log: Vec<LogItem>,
    pub name: Option<String>,
    pub labels: HashMap<String, String>,
    pub stats: SessionStats,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionMetadata {
    pub id: String,
    pub created_at: i64,
    pub parent_session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JsonlSessionMetadata {
    pub id: String,
    pub created_at: i64,
    pub cwd: PathBuf,
    pub path: PathBuf,
    pub modified_at: f64,
    pub source_format: u32,
    pub parent_session_id: Option<String>,
    pub legacy_parent_session_path: Option<PathBuf>,
    pub metadata: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionCreateOptions {
    pub id: Option<String>,
    pub parent_session_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct JsonlSessionCreateOptions {
    pub id: Option<String>,
    pub cwd: PathBuf,
    pub parent_session_id: Option<String>,
    pub metadata: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JsonlSessionListOptions {
    pub cwd: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkPosition {
    Before,
    At,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForkOptions {
    Branch {
        entry_id: Option<String>,
        position: Option<ForkPosition>,
    },
    Tree,
}

impl Default for ForkOptions {
    fn default() -> Self {
        Self::Branch {
            entry_id: None,
            position: None,
        }
    }
}

fn is_false(value: &bool) -> bool {
    !value
}

fn saturating_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use pi_core::{Message, UserMessage};
    use serde_json::json;

    use super::*;

    #[test]
    fn serde_shapes_match_the_typescript_v4_codec() {
        let header = SessionHeader {
            kind: HeaderKind::Header,
            version: 4,
            id: "session".to_string(),
            created_at: 1_700_000_000_000,
            cwd: "/workspace/project".into(),
            parent_session_id: Some("parent".to_string()),
            legacy_parent_session_path: None,
            metadata: Some(serde_json::from_value(json!({"owner": "agent"})).unwrap()),
        };
        assert_eq!(
            serde_json::to_value(header).unwrap(),
            json!({
                "kind": "header",
                "version": 4,
                "id": "session",
                "createdAt": 1_700_000_000_000_i64,
                "cwd": "/workspace/project",
                "parentSessionId": "parent",
                "metadata": {"owner": "agent"}
            })
        );

        let entry = SessionMutation::Entry {
            lane: Some("main".to_string()),
            record: SessionRecord {
                id: "entry-1".to_string(),
                seq: 1,
                parent_id: None,
                timestamp_ms: 100,
                entry: SessionEntry::Custom(CustomEntry {
                    custom_type: "note".to_string(),
                    data: Some(json!({"text": "hello"})),
                }),
            },
        };
        assert_eq!(
            serde_json::to_value(entry).unwrap(),
            json!({
                "kind": "entry",
                "lane": "main",
                "type": "custom",
                "id": "entry-1",
                "seq": 1,
                "parentId": null,
                "timestamp": 100,
                "customType": "note",
                "data": {"text": "hello"}
            })
        );

        let started = SessionMutation::Record {
            record: LaneRecord {
                id: "run-1".to_string(),
                seq: 2,
                lane: "main".to_string(),
                timestamp_ms: 101,
                record: LaneRecordEntry::OperationStarted {
                    source_leaf_id: None,
                    intent: OperationIntent::Run {
                        original_prompt: Vec::new(),
                        initial_messages: Vec::new(),
                        system_prompt_override: None,
                        resume_data: None,
                    },
                },
            },
        };
        assert_eq!(
            serde_json::to_value(started).unwrap(),
            json!({
                "kind": "record",
                "type": "operation_started",
                "id": "run-1",
                "seq": 2,
                "lane": "main",
                "timestamp": 101,
                "sourceLeafId": null,
                "intent": {"kind": "run", "originalPrompt": [], "initialMessages": []}
            })
        );

        assert_eq!(
            serde_json::to_value(SessionMutation::Fact {
                seq: 3,
                fact: SessionFact::Name { name: None },
            })
            .unwrap(),
            json!({"kind": "fact", "seq": 3, "fact": "name"})
        );
    }

    #[test]
    fn decodes_typescript_user_string_content_and_v4_message_entry() {
        let mutation: SessionMutation = serde_json::from_value(json!({
            "kind": "entry",
            "lane": "main",
            "type": "message",
            "id": "message",
            "seq": 1,
            "parentId": null,
            "timestamp": 10,
            "message": {"role": "user", "content": "hello", "timestamp": 9}
        }))
        .unwrap();
        let SessionMutation::Entry { record, .. } = mutation else {
            panic!("expected entry mutation")
        };
        let SessionEntry::Message(message) = record.entry else {
            panic!("expected message entry")
        };
        assert!(matches!(
            message.message.as_standard(),
            Some(Message::User(UserMessage { content, timestamp_ms: 9 })) if content.len() == 1
        ));

        let extension = json!({
            "role": "custom",
            "customType": "notice",
            "content": "plugin context",
            "display": true,
            "details": {"source": "plugin"},
            "timestamp": 11
        });
        let decoded: AgentMessage = serde_json::from_value(extension.clone()).unwrap();
        assert_eq!(decoded.role(), "custom");
        assert_eq!(serde_json::to_value(decoded).unwrap(), extension);

        let future_standard = json!({
            "role": "user",
            "content": "keep the compact string form",
            "timestamp": 12,
            "futureField": {"preserved": true}
        });
        let decoded: AgentMessage = serde_json::from_value(future_standard.clone()).unwrap();
        assert!(matches!(decoded.as_standard(), Some(Message::User(_))));
        assert_eq!(serde_json::to_value(decoded).unwrap(), future_standard);
    }
}
