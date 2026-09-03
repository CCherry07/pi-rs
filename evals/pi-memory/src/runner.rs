use std::time::{Duration, Instant};

use crate::metrics::{score_case, summarize};
use crate::report::REPORT_SCHEMA_VERSION;
use crate::{CaseStatus, EvalBackend, EvalInput, EvalReport, EvalSuite};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunnerConfig {
    pub timeout: Duration,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_millis(50),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct EvalRunner {
    config: RunnerConfig,
}

impl EvalRunner {
    pub fn new(config: RunnerConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> RunnerConfig {
        self.config
    }

    pub async fn run<B>(
        &self,
        backend_name: impl Into<String>,
        suite: &EvalSuite,
        backend: &B,
    ) -> EvalReport
    where
        B: EvalBackend + ?Sized,
    {
        let mut cases = Vec::with_capacity(suite.questions().len());
        for question in suite.questions() {
            let input = EvalInput::from_question(question);
            let started = Instant::now();
            let result = tokio::time::timeout(self.config.timeout, backend.gather(input)).await;
            let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
            let (status, hits, candidate_trace) = match result {
                Ok(Ok(observation)) => (
                    CaseStatus::Completed,
                    observation.hits,
                    observation.candidate_trace,
                ),
                Ok(Err(error)) => (
                    CaseStatus::BackendError {
                        message: error.message().to_string(),
                    },
                    Vec::new(),
                    None,
                ),
                Err(_) => (CaseStatus::TimedOut, Vec::new(), None),
            };
            cases.push(score_case(
                question,
                status,
                elapsed_ms,
                hits,
                candidate_trace,
            ));
        }
        let summary = summarize(&cases);
        EvalReport {
            schema_version: REPORT_SCHEMA_VERSION,
            corpus: suite.corpus_name().to_string(),
            corpus_version: suite.corpus_version().to_string(),
            corpus_seed: suite.seed(),
            suite: suite.name().to_string(),
            haystack: suite.haystack().to_string(),
            backend: backend_name.into(),
            timeout_ms: duration_millis(self.config.timeout),
            sessions: suite.session_count(),
            records: suite.record_count(),
            summary,
            cases,
        }
    }
}

impl Default for EvalRunner {
    fn default() -> Self {
        Self::new(RunnerConfig::default())
    }
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
