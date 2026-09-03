use std::time::Duration;

use async_trait::async_trait;
use pi_memory_eval::{
    EvalBackend, EvalBackendError, EvalCandidateTrace, EvalCorpus, EvalInput, EvalObservation,
    EvalRankingStageTrace, EvalRunner, RunnerConfig,
};

struct CandidateTraceBackend;

#[async_trait]
impl EvalBackend for CandidateTraceBackend {
    async fn gather(&self, input: EvalInput) -> Result<EvalObservation, EvalBackendError> {
        let candidate_trace =
            (input.query == "Atlas 完整 test command cargo workspace").then(|| {
                EvalCandidateTrace {
                    sparse_record_ids: vec![
                        "atlas-test-command".to_string(),
                        "sparse-noise".to_string(),
                    ],
                    dense_record_ids: vec!["dense-noise".to_string()],
                    ranking_stages: Some(EvalRankingStageTrace {
                        protected_core_record_ids: vec!["sparse-noise".to_string()],
                        gate_eligible_record_ids: vec![
                            "sparse-noise".to_string(),
                            "atlas-test-command".to_string(),
                        ],
                        pre_cutoff_record_ids: vec![
                            "sparse-noise".to_string(),
                            "atlas-test-command".to_string(),
                        ],
                    }),
                }
            });
        Ok(EvalObservation {
            hits: Vec::new(),
            candidate_trace,
        })
    }
}

#[tokio::test]
async fn report_distinguishes_candidate_coverage_from_final_recall() {
    let corpus = EvalCorpus::bundled().expect("bundled corpus");
    let suite = corpus.suite("smoke").expect("smoke suite");
    let runner = EvalRunner::new(RunnerConfig {
        timeout: Duration::from_millis(500),
    });

    let report = runner
        .run("candidate-trace", &suite, &CandidateTraceBackend)
        .await;
    let case = report
        .cases
        .iter()
        .find(|case| case.question_id == "static-atlas-full-test-command")
        .expect("test-command case");
    let coverage = case
        .candidate_coverage
        .as_ref()
        .expect("candidate coverage");

    assert_eq!(case.recall_at_8, 0.0);
    assert_eq!(coverage.sparse.candidate_count, 2);
    assert_eq!(coverage.sparse.recall, 1.0);
    assert_eq!(coverage.dense.candidate_count, 1);
    assert_eq!(coverage.dense.recall, 0.0);
    assert_eq!(coverage.union.candidate_count, 3);
    assert_eq!(coverage.union.recall, 1.0);
    assert!(coverage.union.all_hops);
    let ranking_stages = coverage
        .ranking_stages
        .as_ref()
        .expect("ranking-stage coverage");
    assert_eq!(ranking_stages.protected_core.recall, 0.0);
    assert_eq!(ranking_stages.gate_eligible.recall, 1.0);
    assert_eq!(ranking_stages.pre_cutoff.recall, 1.0);

    let summary = report
        .summary
        .candidate_coverage
        .expect("summary candidate coverage");
    assert_eq!(summary.cases, 1);
    assert_eq!(summary.sparse_recall, 1.0);
    assert_eq!(summary.dense_recall, 0.0);
    assert_eq!(summary.union_recall, 1.0);
    assert_eq!(summary.union_all_hops_rate, 1.0);
    let ranking_stages = summary
        .ranking_stages
        .expect("summary ranking-stage coverage");
    assert_eq!(ranking_stages.cases, 1);
    assert_eq!(ranking_stages.protected_core_recall, 0.0);
    assert_eq!(ranking_stages.gate_eligible_recall, 1.0);
    assert_eq!(ranking_stages.pre_cutoff_recall, 1.0);
}
