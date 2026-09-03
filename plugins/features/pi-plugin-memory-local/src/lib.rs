#![deny(unsafe_code, unsafe_op_in_unsafe_fn)]

//! First-party local semantic-memory plugin and on-device implementation.
//!
//! Record, mutation, query, and ranking types in this crate are local-provider
//! policy. Third-party memory providers integrate through `pi-memory-loader`
//! and Pi's ordinary agent/session plugin lifecycles; they do not implement
//! these storage shapes.

mod commands;
mod embedding;
mod factory;
mod plugin;
mod ranking;
mod runtime;
mod storage;
mod tools;
mod types;

pub use embedding::{
    EmbeddingDescriptor, EmbeddingError, EmbeddingPurpose, FastEmbedInstallReceipt,
    FastEmbedModelError, FastEmbedModelState, FastEmbedModelStatus, FastEmbedModelStore,
    MemoryEmbedder,
};
pub use factory::LocalMemoryProviderFactory;
pub use plugin::LocalMemoryPlugin;
pub use storage::{
    LocalMemoryProvider, MemoryEmbeddingBackfillReceipt, MemoryEmbeddingHealth, MemoryHealth,
    MemoryRebuildBatch, MemoryRebuildReceipt, SqliteRankingStages, SqliteRecallCandidates,
    SqliteRecallRanking,
};
pub use types::{
    ApplyReceipt, MAX_EVIDENCE_BYTES, MAX_MEMORY_TEXT_BYTES, MEMORY_EVENT_TYPE, MemoryError,
    MemoryEvidence, MemoryHit, MemoryKind, MemoryMutation, MemoryOrigin, MemoryRecord, MemoryScope,
    MemoryValidationError, RecallQuery, RecallResult, SessionIndexDocument, SessionIndexEntry,
    SessionSearchHit, SessionSearchQuery,
};

pub const LOCAL_MEMORY_PROVIDER_ID: &str = "local";
pub const LOCAL_MEMORY_PLUGIN_ID: &str = "memory-local";
