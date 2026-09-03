use std::path::PathBuf;
use std::time::Duration;

use pi_memory_eval::{
    EvalCorpus, EvalLanguageRelation, EvalRunner, HopClass, ProviderBackend, RunnerConfig,
    SqliteProviderBackend,
};
use pi_plugin_memory_local::{FastEmbedModelState, FastEmbedModelStore, LocalMemoryProvider};

const CACHE_ENV: &str = "PI_MEMORY_EMBEDDING_CACHE";

#[tokio::test]
#[ignore = "requires an explicitly installed multilingual-e5-small model"]
async fn pinned_dense_model_reproduces_hybrid_small_split_baseline() {
    let cache_dir = PathBuf::from(
        std::env::var_os(CACHE_ENV)
            .unwrap_or_else(|| panic!("set {CACHE_ENV} to the installed embedding cache")),
    );
    let store = FastEmbedModelStore::new(&cache_dir);
    assert!(matches!(
        store.status().state,
        FastEmbedModelState::Ready { .. }
    ));
    let embedder = store
        .embedder_if_ready()
        .expect("valid model installation")
        .expect("installed model");

    let corpus = EvalCorpus::bundled().expect("bundled corpus");
    let dev = corpus.suite("small-dev").expect("small dev suite");
    let holdout = corpus.suite("small-holdout").expect("small holdout suite");
    let directory = tempfile::tempdir().expect("temporary directory");
    let provider =
        LocalMemoryProvider::open_with_embedder(directory.path().join("memory.sqlite3"), embedder)
            .expect("dense SQLite provider");
    provider
        .apply(dev.mutations())
        .await
        .expect("embed fixed-seed haystack");

    let backend = ProviderBackend::new(provider);
    let runner = EvalRunner::new(RunnerConfig {
        timeout: Duration::from_millis(500),
    });
    let dev_report = runner.run("sqlite-dense", &dev, &backend).await;
    let holdout_report = runner.run("sqlite-dense", &holdout, &backend).await;

    for report in [&dev_report, &holdout_report] {
        assert_eq!(report.summary.completed, 15);
        assert_eq!(report.summary.timed_out, 0);
        assert_eq!(report.summary.backend_errors, 0);
        assert_eq!(report.summary.retrieval.wrong_scope_hits, 0);
        assert_eq!(report.summary.retrieval.stale_hits, 0);
        assert_eq!(report.summary.retrieval.distractor_hits, 0);
    }
    assert_eq!(dev_report.summary.retrieval.recall_at_5, 1.0);
    assert_eq!(dev_report.summary.retrieval.all_hops_rate, 1.0);
    assert_eq!(holdout_report.summary.retrieval.recall_at_5, 14.0 / 15.0);
    assert_eq!(holdout_report.summary.retrieval.all_hops_rate, 14.0 / 15.0);
    assert_eq!(
        holdout_report
            .summary
            .by_language_relation
            .get(&EvalLanguageRelation::CrossLanguage)
            .expect("cross-language slice")
            .recall_at_5,
        1.0
    );
    assert_eq!(
        holdout_report
            .summary
            .by_hop_class
            .get(&HopClass::MultiHop)
            .expect("multi-hop slice")
            .all_hops_rate,
        1.0
    );
}

#[tokio::test]
#[ignore = "requires an explicitly installed multilingual-e5-small model"]
async fn pinned_dense_model_solves_ranking_development_slices_at_both_scales() {
    let cache_dir = PathBuf::from(
        std::env::var_os(CACHE_ENV)
            .unwrap_or_else(|| panic!("set {CACHE_ENV} to the installed embedding cache")),
    );
    let store = FastEmbedModelStore::new(&cache_dir);
    assert!(matches!(
        store.status().state,
        FastEmbedModelState::Ready { .. }
    ));
    let embedder = store
        .embedder_if_ready()
        .expect("valid model installation")
        .expect("installed model");
    let corpus = EvalCorpus::bundled().expect("bundled corpus");

    for suite_name in ["small-ranking-dev", "medium-ranking-dev"] {
        let suite = corpus.suite(suite_name).expect("ranking development suite");
        let directory = tempfile::tempdir().expect("temporary directory");
        let provider = LocalMemoryProvider::open_with_embedder(
            directory.path().join("memory.sqlite3"),
            embedder.clone(),
        )
        .expect("dense SQLite provider");
        provider
            .apply(suite.mutations())
            .await
            .expect("embed fixed-seed haystack");

        let report = EvalRunner::new(RunnerConfig {
            timeout: Duration::from_millis(1_000),
        })
        .run("sqlite-dense", &suite, &ProviderBackend::new(provider))
        .await;

        assert_eq!(report.summary.completed, 6, "{suite_name}");
        assert_eq!(report.summary.timed_out, 0, "{suite_name}");
        assert_eq!(report.summary.backend_errors, 0, "{suite_name}");
        assert_eq!(report.summary.retrieval.recall_at_5, 1.0, "{suite_name}");
        assert_eq!(report.summary.retrieval.all_hops_rate, 1.0, "{suite_name}");
        assert_eq!(report.summary.retrieval.wrong_scope_hits, 0, "{suite_name}");
        assert_eq!(report.summary.retrieval.stale_hits, 0, "{suite_name}");
        assert_eq!(report.summary.retrieval.distractor_hits, 0, "{suite_name}");
    }
}

#[tokio::test]
#[ignore = "requires an explicitly installed multilingual-e5-small model"]
async fn long_semantic_queries_reach_the_medium_candidate_union() {
    let cache_dir = PathBuf::from(
        std::env::var_os(CACHE_ENV)
            .unwrap_or_else(|| panic!("set {CACHE_ENV} to the installed embedding cache")),
    );
    let embedder = FastEmbedModelStore::new(&cache_dir)
        .embedder_if_ready()
        .expect("valid model installation")
        .expect("installed model");
    let corpus = EvalCorpus::bundled().expect("bundled corpus");
    let suite = corpus
        .suite("medium-ranking-dev")
        .expect("medium ranking development suite");
    let directory = tempfile::tempdir().expect("temporary directory");
    let provider =
        LocalMemoryProvider::open_with_embedder(directory.path().join("memory.sqlite3"), embedder)
            .expect("dense SQLite provider");
    provider
        .apply(suite.mutations())
        .await
        .expect("embed fixed-seed haystack");

    let report = EvalRunner::new(RunnerConfig {
        timeout: Duration::from_millis(1_000),
    })
    .run(
        "sqlite-dense",
        &suite,
        &SqliteProviderBackend::new(provider),
    )
    .await;

    for question_id in [
        "ranking-dev-premise-command-existence",
        "ranking-dev-procedure-promotion-and-verification",
    ] {
        let case = report
            .cases
            .iter()
            .find(|case| case.question_id == question_id)
            .expect("candidate-saturation development case");
        assert_eq!(
            case.candidate_coverage
                .as_ref()
                .expect("candidate trace")
                .union
                .recall,
            1.0,
            "{question_id}"
        );
    }
}

#[tokio::test]
#[ignore = "requires an explicitly installed multilingual-e5-small model"]
async fn selected_semantic_evidence_survives_the_final_score_cutoff() {
    let cache_dir = PathBuf::from(
        std::env::var_os(CACHE_ENV)
            .unwrap_or_else(|| panic!("set {CACHE_ENV} to the installed embedding cache")),
    );
    let embedder = FastEmbedModelStore::new(&cache_dir)
        .embedder_if_ready()
        .expect("valid model installation")
        .expect("installed model");
    let corpus = EvalCorpus::bundled().expect("bundled corpus");

    for suite_name in ["small-ranking-dev", "medium-ranking-dev"] {
        let suite = corpus.suite(suite_name).expect("ranking development suite");
        let directory = tempfile::tempdir().expect("temporary directory");
        let provider = LocalMemoryProvider::open_with_embedder(
            directory.path().join("memory.sqlite3"),
            embedder.clone(),
        )
        .expect("dense SQLite provider");
        provider
            .apply(suite.mutations())
            .await
            .expect("embed fixed-seed haystack");
        let report = EvalRunner::new(RunnerConfig {
            timeout: Duration::from_millis(1_000),
        })
        .run(
            "sqlite-dense",
            &suite,
            &SqliteProviderBackend::new(provider),
        )
        .await;

        for (question_id, evidence_id) in [
            (
                "ranking-dev-static-canonical-workspace-test",
                "atlas-test-command",
            ),
            ("ranking-dev-premise-tool-inventory", "atlas-tools-list"),
        ] {
            let case = report
                .cases
                .iter()
                .find(|case| case.question_id == question_id)
                .expect("final-cutoff development case");
            assert_eq!(
                case.candidate_coverage
                    .as_ref()
                    .and_then(|coverage| coverage.ranking_stages.as_ref())
                    .expect("ranking-stage trace")
                    .pre_cutoff
                    .recall,
                1.0,
                "{suite_name}: {question_id} must reach pre-cutoff"
            );
            assert!(
                case.hit_record_ids
                    .iter()
                    .any(|record_id| record_id == evidence_id),
                "{suite_name}: {question_id}"
            );
        }
    }
}

#[tokio::test]
#[ignore = "requires an explicitly installed multilingual-e5-small model"]
async fn query_facets_admit_complementary_semantic_evidence() {
    let cache_dir = PathBuf::from(
        std::env::var_os(CACHE_ENV)
            .unwrap_or_else(|| panic!("set {CACHE_ENV} to the installed embedding cache")),
    );
    let embedder = FastEmbedModelStore::new(&cache_dir)
        .embedder_if_ready()
        .expect("valid model installation")
        .expect("installed model");
    let corpus = EvalCorpus::bundled().expect("bundled corpus");

    for suite_name in ["small-ranking-dev", "medium-ranking-dev"] {
        let suite = corpus.suite(suite_name).expect("ranking development suite");
        let directory = tempfile::tempdir().expect("temporary directory");
        let provider = LocalMemoryProvider::open_with_embedder(
            directory.path().join("memory.sqlite3"),
            embedder.clone(),
        )
        .expect("dense SQLite provider");
        provider
            .apply(suite.mutations())
            .await
            .expect("embed fixed-seed haystack");
        let report = EvalRunner::new(RunnerConfig {
            timeout: Duration::from_millis(1_000),
        })
        .run(
            "sqlite-dense",
            &suite,
            &SqliteProviderBackend::new(provider),
        )
        .await;

        for question_id in [
            "ranking-dev-premise-command-existence",
            "ranking-dev-procedure-promotion-and-verification",
        ] {
            let case = report
                .cases
                .iter()
                .find(|case| case.question_id == question_id)
                .expect("query-facet development case");
            let stages = case
                .candidate_coverage
                .as_ref()
                .and_then(|coverage| coverage.ranking_stages.as_ref())
                .expect("ranking-stage trace");
            assert_eq!(
                stages.gate_eligible.recall, 1.0,
                "{suite_name}: {question_id}"
            );
            assert_eq!(stages.pre_cutoff.recall, 1.0, "{suite_name}: {question_id}");
        }
    }
}
