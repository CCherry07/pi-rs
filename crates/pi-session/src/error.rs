//! Runtime-facing session failures. Durable errors are translated at this seam.
use pi_core::session::SessionWireError;

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
    Plugin(#[from] crate::PluginError),
    #[error("session is busy with another operation")]
    Busy,
    #[error("session is closed")]
    Closed,
    #[error("{0} cancelled")]
    Cancelled(&'static str),
}

impl From<pi_runtime::RuntimeError> for SessionError {
    fn from(error: pi_runtime::RuntimeError) -> Self {
        Self::Runtime(error.to_string())
    }
}

impl From<SessionWireError> for SessionError {
    fn from(error: SessionWireError) -> Self {
        match error {
            SessionWireError::InvalidPayload(message) => Self::InvalidPayload(message),
        }
    }
}

impl From<crate::history::HistoryError> for SessionError {
    fn from(error: crate::history::HistoryError) -> Self {
        use crate::history::HistoryError;
        match error {
            HistoryError::Io(error) => Self::Io(error),
            HistoryError::InvalidJson { line, message } => Self::InvalidJson { line, message },
            HistoryError::MissingHeader => Self::MissingHeader,
            HistoryError::UnsupportedSchema(version) => Self::UnsupportedSchema(version),
            HistoryError::AlreadyExists(value) => Self::AlreadyExists(value),
            HistoryError::NotFound(value) => Self::NotFound(value),
            HistoryError::InvalidEntry(value) => Self::InvalidEntry(value),
            HistoryError::InvalidPayload(value) => Self::InvalidPayload(value),
            HistoryError::InvalidLane(value) => Self::InvalidLane(value),
            HistoryError::InvalidQuery(value) => Self::InvalidQuery(value),
            HistoryError::InvalidForkTarget(value) => Self::InvalidForkTarget(value),
            HistoryError::Storage(value) => Self::Storage(value),
        }
    }
}
