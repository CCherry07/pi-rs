//! Domain-neutral agent evaluation over the shared managed-session lifecycle.
//!
//! Applications prepare their own target, prompt bindings, and optional prompt
//! transformation. The runner owns fixtures, steps, grading, and artifacts.

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
pub use harness::{EvalPromptTransform, EvalRunContext, EvalRunner, PreparedEvalTarget};
pub use model::{
    ArtifactReference, EvalAgentPluginFactory, EvalCase, EvalError, EvalExecutionOutcome,
    EvalFixture, EvalGrade, EvalLimits, EvalObservation, EvalRun, EvalStep, EvalTranscriptEvent,
    EvalUsage, EvalVariant, WorkspaceChange, WorkspaceChangeKind,
};
pub use submission::{JsonSubmissionGrader, JsonSubmissionPlugin};
