#![forbid(unsafe_code)]

//! Deterministic retrieval evaluation for `pi-memory` providers.
//!
//! The benchmark-facing [`EvalBackend`] sees only [`EvalInput`]. Gold evidence,
//! forbidden records, abilities, and expected answers remain inside the runner.

mod backend;
mod corpus;
mod error;
mod filler;
mod metrics;
mod report;
mod runner;

pub use backend::{
    EvalBackend, EvalBackendError, EvalCandidateTrace, EvalHit, EvalInput, EvalObservation,
    EvalRankingStageTrace, NoRecallBackend, OracleBackend, ProviderBackend, SqliteProviderBackend,
};
pub use corpus::{
    CorpusManifest, EvalAbility, EvalCorpus, EvalLanguageRelation, EvalQuestion, EvalSession,
    EvalSuite, EvalSuiteSpec, ForbiddenReason, ForbiddenRecord, HaystackSpec,
};
pub use error::EvalError;
pub use report::{
    CandidateCoverage, CandidateCoverageMetrics, CandidateSetCoverage, CaseReport, CaseStatus,
    EvalReport, EvalSummary, ForbiddenHit, HopClass, LatencyMetrics, RankingStageCoverage,
    RankingStageCoverageMetrics, RetrievalMetrics, SliceMetrics,
};
pub use runner::{EvalRunner, RunnerConfig};
