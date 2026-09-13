//! Model-backed evaluation support for the Pi product runtime.
//!
//! This crate is an outer test/product-quality layer. Production crates do not
//! depend on it; the harness deliberately enters through [`pi_sdk::Pi`].

#![forbid(unsafe_code)]

mod artifact;
mod comparison;
mod grader;
mod harness;
mod model;
mod snapshot;
mod submission;

pub use artifact::ArtifactStore;
pub use comparison::{
    ComparisonDiagnosticReason, CorrectnessLiftSummary, EvalComparisonDefinition,
    HarnessComparisonDiagnostic, HarnessComparisonReport, HarnessEvalSetReport,
    HarnessPairComparison, PairedMetricSummary, format_comparison_report, summarize_comparisons,
};
pub use grader::{EvalGrader, ExactOutputGrader};
pub use harness::PiEvalHarness;
pub use model::{
    ArtifactReference, EvalAgentPluginFactory, EvalCase, EvalError, EvalExecutionOutcome,
    EvalFixture, EvalGrade, EvalLimits, EvalObservation, EvalRun, EvalStep, EvalSystemPrompt,
    EvalTranscriptEvent, EvalUsage, EvalVariant, WorkspaceChange, WorkspaceChangeKind,
};
pub use submission::{JsonSubmissionGrader, JsonSubmissionPlugin};
