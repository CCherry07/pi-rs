//! Coding product preparation for the shared evaluation runner.
//!
//! This adapter uses the same [`pi_coding::Pi`] host as other product frontends.
//! [`pi_eval::EvalRunner`] owns execution, observation, grading, and artifacts.

#![forbid(unsafe_code)]

mod harness;
mod model;

pub use harness::CodingEvalHarness;
pub use model::{CodingEvalCase, CodingEvalOptions, CodingEvalSystemPrompt, CodingEvalVariant};
