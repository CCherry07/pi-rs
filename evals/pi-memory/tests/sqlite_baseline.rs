use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pi_memory_eval::{
    EvalCorpus, EvalLanguageRelation, EvalRunner, HopClass, ProviderBackend, RunnerConfig,
    SqliteProviderBackend,
};
use pi_plugin_memory_local::{
    EmbeddingDescriptor, EmbeddingError, EmbeddingPurpose, LocalMemoryProvider, MemoryEmbedder,
    RecallQuery, SqliteRecallRanking,
};

#[derive(Debug)]
struct SparseOnlyEvidenceTestEmbedder {
    descriptor: EmbeddingDescriptor,
}

impl SparseOnlyEvidenceTestEmbedder {
    fn new() -> Self {
        Self {
            descriptor: EmbeddingDescriptor {
                model: "sparse-only-evidence-test".to_string(),
                revision: "v1".to_string(),
                dimensions: 2,
            },
        }
    }
}

#[async_trait]
impl MemoryEmbedder for SparseOnlyEvidenceTestEmbedder {
    fn descriptor(&self) -> &EmbeddingDescriptor {
        &self.descriptor
    }

    async fn embed(
        &self,
        purpose: EmbeddingPurpose,
        texts: Vec<String>,
    ) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        Ok(texts
            .into_iter()
            .map(|text| match purpose {
                EmbeddingPurpose::Query => vec![1.0, 0.0],
                EmbeddingPurpose::Document if text.contains("cargo test --workspace") => {
                    vec![-1.0, 0.0]
                }
                EmbeddingPurpose::Document => vec![1.0, 0.0],
            })
            .collect())
    }
}

#[tokio::test]
async fn medium_hybrid_rescue_keeps_complementary_workspace_command() {
    let corpus = EvalCorpus::bundled().expect("bundled corpus");
    let suite = corpus.suite("medium-dev").expect("medium dev suite");
    let question = suite
        .questions()
        .iter()
        .find(|question| question.id == "procedure-atlas-release")
        .expect("release question");
    let directory = tempfile::tempdir().expect("temporary directory");
    let provider = LocalMemoryProvider::open_with_embedder(
        directory.path().join("memory.sqlite3"),
        Arc::new(SparseOnlyEvidenceTestEmbedder::new()),
    )
    .expect("dense SQLite provider");
    provider
        .apply(suite.mutations())
        .await
        .expect("seed medium mutations");

    let traced = provider
        .recall_with_candidates(RecallQuery {
            text: question.query.clone(),
            scopes: question.scopes.clone(),
            limit: question.limit,
        })
        .await
        .expect("traced recall");
    let stages = traced.ranking_stages.expect("hybrid ranking stages");

    assert!(
        !stages
            .protected_core_record_ids
            .iter()
            .any(|record_id| record_id == "atlas-test-command")
    );
    assert!(
        !traced
            .dense_record_ids
            .iter()
            .any(|record_id| record_id == "atlas-test-command")
    );
    assert!(
        stages
            .gate_eligible_record_ids
            .iter()
            .any(|record_id| record_id == "atlas-test-command")
    );
    let selected = stages
        .pre_cutoff_record_ids
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    assert!(
        traced
            .result
            .hits
            .iter()
            .any(|hit| hit.record.id == "atlas-test-command"),
        "pre-cutoff selection: {selected:#?}"
    );
}

#[tokio::test]
async fn sqlite_adapter_reports_pre_ranking_candidate_coverage() {
    let corpus = EvalCorpus::bundled().expect("bundled corpus");
    let suite = corpus.suite("smoke").expect("smoke suite");
    let directory = tempfile::tempdir().expect("temporary directory");
    let provider = LocalMemoryProvider::open(directory.path().join("memory.sqlite3"))
        .expect("SQLite provider");
    provider
        .apply(suite.mutations())
        .await
        .expect("seed mutations");
    let runner = EvalRunner::new(RunnerConfig {
        timeout: Duration::from_millis(500),
    });

    let report = runner
        .run("sqlite", &suite, &SqliteProviderBackend::new(provider))
        .await;
    let summary = report
        .summary
        .candidate_coverage
        .expect("candidate coverage summary");
    let test_command = report
        .cases
        .iter()
        .find(|case| case.question_id == "static-atlas-full-test-command")
        .and_then(|case| case.candidate_coverage.as_ref())
        .expect("test command candidate coverage");

    assert_eq!(summary.cases, 15);
    assert_eq!(test_command.sparse.recall, 1.0);
    assert_eq!(test_command.dense.candidate_count, 0);
    assert_eq!(test_command.union.recall, 1.0);
}

#[tokio::test]
async fn sqlite_smoke_baseline_retrieves_evidence_without_scope_or_stale_leaks() {
    let corpus = EvalCorpus::bundled().expect("bundled corpus");
    let suite = corpus.suite("smoke").expect("smoke suite");
    let directory = tempfile::tempdir().expect("temporary directory");
    let provider = LocalMemoryProvider::open(directory.path().join("memory.sqlite3"))
        .expect("SQLite provider");
    provider
        .apply(suite.mutations())
        .await
        .expect("seed mutations");
    let runner = EvalRunner::new(RunnerConfig {
        timeout: Duration::from_millis(500),
    });

    let report = runner
        .run("sqlite", &suite, &ProviderBackend::new(provider))
        .await;

    assert_eq!(report.summary.completed, suite.questions().len());
    assert_eq!(report.summary.timed_out, 0);
    assert_eq!(report.summary.backend_errors, 0);
    assert!(report.summary.retrieval.recall_at_5 >= 0.95);
    assert!(report.summary.retrieval.all_hops_rate >= 0.95);
    assert_eq!(report.summary.retrieval.wrong_scope_hits, 0);
    assert_eq!(report.summary.retrieval.stale_hits, 0);
}

#[tokio::test]
async fn hybrid_small_dev_improves_recall_and_context_density_over_bm25() {
    let corpus = EvalCorpus::bundled().expect("bundled corpus");
    let suite = corpus.suite("small-dev").expect("small dev suite");
    let directory = tempfile::tempdir().expect("temporary directory");
    let bm25 = LocalMemoryProvider::open_with_ranking(
        directory.path().join("bm25.sqlite3"),
        SqliteRecallRanking::Bm25,
    )
    .expect("BM25 provider");
    let hybrid = LocalMemoryProvider::open_with_ranking(
        directory.path().join("hybrid.sqlite3"),
        SqliteRecallRanking::Hybrid,
    )
    .expect("hybrid provider");
    for provider in [&bm25, &hybrid] {
        provider
            .apply(suite.mutations())
            .await
            .expect("seed mutations");
    }
    let runner = EvalRunner::new(RunnerConfig {
        timeout: Duration::from_millis(500),
    });

    let bm25_report = runner
        .run("sqlite-bm25", &suite, &ProviderBackend::new(bm25))
        .await;
    let hybrid_report = runner
        .run("sqlite", &suite, &ProviderBackend::new(hybrid))
        .await;

    assert_eq!(bm25_report.summary.retrieval.recall_at_5, 0.9);
    assert_eq!(hybrid_report.summary.retrieval.recall_at_5, 29.0 / 30.0);
    assert_eq!(hybrid_report.summary.retrieval.all_hops_rate, 14.0 / 15.0);
    assert!(
        hybrid_report.summary.retrieval.recall_at_5 > bm25_report.summary.retrieval.recall_at_5
    );
    assert!(
        hybrid_report.summary.retrieval.evidence_density
            > bm25_report.summary.retrieval.evidence_density
    );
    assert_eq!(hybrid_report.summary.retrieval.wrong_scope_hits, 0);
    assert_eq!(hybrid_report.summary.retrieval.stale_hits, 0);
    assert_eq!(hybrid_report.summary.retrieval.distractor_hits, 0);
    assert!(
        hybrid_report
            .cases
            .iter()
            .map(|case| case.returned_hit_count)
            .sum::<usize>()
            < bm25_report
                .cases
                .iter()
                .map(|case| case.returned_hit_count)
                .sum()
    );
}

#[tokio::test]
async fn frozen_holdout_exposes_cross_language_and_multi_hop_gaps() {
    let corpus = EvalCorpus::bundled().expect("bundled corpus");
    let suite = corpus.suite("small-holdout").expect("small holdout suite");
    let directory = tempfile::tempdir().expect("temporary directory");
    let bm25 = LocalMemoryProvider::open_with_ranking(
        directory.path().join("bm25.sqlite3"),
        SqliteRecallRanking::Bm25,
    )
    .expect("BM25 provider");
    let hybrid = LocalMemoryProvider::open_with_ranking(
        directory.path().join("hybrid.sqlite3"),
        SqliteRecallRanking::Hybrid,
    )
    .expect("hybrid provider");
    for provider in [&bm25, &hybrid] {
        provider
            .apply(suite.mutations())
            .await
            .expect("seed mutations");
    }
    let runner = EvalRunner::new(RunnerConfig {
        timeout: Duration::from_millis(500),
    });

    let bm25_report = runner
        .run("sqlite-bm25", &suite, &ProviderBackend::new(bm25))
        .await;
    let hybrid_report = runner
        .run("sqlite", &suite, &ProviderBackend::new(hybrid))
        .await;

    assert_eq!(bm25_report.summary.retrieval.recall_at_5, 11.0 / 15.0);
    assert_eq!(hybrid_report.summary.retrieval.recall_at_5, 23.0 / 30.0);
    assert_eq!(hybrid_report.summary.retrieval.all_hops_rate, 11.0 / 15.0);
    assert_eq!(
        hybrid_report
            .summary
            .by_language_relation
            .get(&EvalLanguageRelation::SameLanguage)
            .expect("same-language slice")
            .recall_at_5,
        1.0
    );
    assert_eq!(
        hybrid_report
            .summary
            .by_language_relation
            .get(&EvalLanguageRelation::CrossLanguage)
            .expect("cross-language slice")
            .recall_at_5,
        0.75
    );
    assert_eq!(
        hybrid_report
            .summary
            .by_language_relation
            .get(&EvalLanguageRelation::MixedLanguage)
            .expect("mixed-language slice")
            .recall_at_5,
        0.25
    );
    assert_eq!(
        hybrid_report
            .summary
            .by_hop_class
            .get(&HopClass::MultiHop)
            .expect("multi-hop slice")
            .all_hops_rate,
        0.0
    );
    assert_eq!(hybrid_report.summary.retrieval.wrong_scope_hits, 0);
    assert_eq!(hybrid_report.summary.retrieval.stale_hits, 0);
    assert_eq!(hybrid_report.summary.retrieval.distractor_hits, 0);
    assert!(
        hybrid_report.summary.retrieval.evidence_density
            > bm25_report.summary.retrieval.evidence_density
    );
}
