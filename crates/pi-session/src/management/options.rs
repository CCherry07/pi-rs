//! Acquisition options for managed sessions.
use crate::{SessionGenerationOverlay, SessionStore};
use std::path::PathBuf;

/// Required location and optional metadata for a new managed session.
#[derive(Debug, Clone)]
pub struct CreateSessionOptions {
    pub workspace: pi_core::WorkspaceSpec,
    pub path: PathBuf,
    pub session_id: Option<String>,
    pub metadata: Option<serde_json::Map<String, serde_json::Value>>,
    /// Transient factories rebuilt on every generation of the returned handle.
    pub generation_overlay: SessionGenerationOverlay,
}

impl CreateSessionOptions {
    pub fn new(cwd: impl Into<PathBuf>, path: impl Into<PathBuf>) -> Self {
        Self {
            workspace: pi_core::WorkspaceSpec::from_cwd(cwd),
            path: path.into(),
            session_id: None,
            metadata: None,
            generation_overlay: SessionGenerationOverlay::default(),
        }
    }
}

/// A saved path or a history store the caller has already prepared and validated.
/// Passing a store avoids parsing its JSONL file a second time.
#[derive(Debug, Clone)]
pub enum SessionSource {
    Path(PathBuf),
    Store(SessionStore),
}

impl<P: Into<PathBuf>> From<P> for SessionSource {
    fn from(path: P) -> Self {
        Self::Path(path.into())
    }
}

/// Transient additions used when opening a managed session.
#[derive(Debug, Clone, Default)]
pub struct OpenSessionOptions {
    pub generation_overlay: SessionGenerationOverlay,
}
