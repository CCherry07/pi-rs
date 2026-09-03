use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryRecallOptions {
    pub max_records: usize,
    pub token_budget: usize,
    pub timeout: Duration,
}

impl Default for MemoryRecallOptions {
    fn default() -> Self {
        Self {
            max_records: 8,
            token_budget: 1_200,
            timeout: Duration::from_millis(50),
        }
    }
}

/// Host-owned inputs used to locate and initialize a memory provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryLoaderOptions {
    pub cwd: PathBuf,
    pub agent_dir: PathBuf,
    pub session_roots: Vec<PathBuf>,
    pub recall_options: MemoryRecallOptions,
}

impl MemoryLoaderOptions {
    pub fn new(cwd: impl Into<PathBuf>, agent_dir: impl Into<PathBuf>) -> Self {
        let agent_dir = agent_dir.into();
        Self {
            cwd: cwd.into(),
            agent_dir: agent_dir.clone(),
            session_roots: vec![agent_dir.join("sessions")],
            recall_options: MemoryRecallOptions::default(),
        }
    }
}
