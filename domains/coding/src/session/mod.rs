//! Coding-specific session adapters selected by each product generation.

pub mod compaction;
pub mod shell;

pub use compaction::CodingCompactionPolicy;
pub use shell::CodingShellExecutor;
