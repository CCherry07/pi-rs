use std::time::Duration;

use pi_memory_eval::{
    EvalAbility, EvalCorpus, EvalLanguageRelation, EvalRunner, HopClass, NoRecallBackend,
    OracleBackend, RunnerConfig,
};
use pi_plugin_memory_local::MemoryMutation;

#[test]
fn holdout_v2_isolated_queries_reuse_the_frozen_small_and_medium_haystacks() {
    let corpus = EvalCorpus::bundled().expect("bundled corpus");
    let small_dev = corpus.suite("small-dev").expect("small dev suite");
    let small_holdout = corpus.suite("small-holdout").expect("small holdout suite");
    let small_v2 = corpus
        .suite("small-holdout-v2")
        .expect("small holdout v2 suite");
    let medium_holdout = corpus
        .suite("medium-holdout")
        .expect("medium holdout suite");
    let medium_v2 = corpus
        .suite("medium-holdout-v2")
        .expect("medium holdout v2 suite");

    assert_eq!(small_v2.haystack(), "small");
    assert_eq!(medium_v2.haystack(), "medium");
    assert_eq!(small_v2.mutations(), small_holdout.mutations());
    assert_eq!(medium_v2.mutations(), medium_holdout.mutations());
    assert_eq!(small_v2.questions(), medium_v2.questions());
    assert_eq!(small_v2.questions().len(), 15);
    assert!(
        small_v2
            .questions()
            .iter()
            .all(|question| question.id.starts_with("holdout-v2-"))
    );
    assert!(small_v2.questions().iter().all(|question| {
        small_dev
            .questions()
            .iter()
            .chain(small_holdout.questions())
            .all(|previous| previous.id != question.id)
    }));
    for ability in [
        EvalAbility::StaticState,
        EvalAbility::DynamicState,
        EvalAbility::Procedure,
        EvalAbility::Gotcha,
        EvalAbility::PremiseAwareness,
    ] {
        assert_eq!(
            small_v2
                .questions()
                .iter()
                .filter(|question| question.ability == ability)
                .count(),
            3
        );
    }
    assert_eq!(
        small_v2
            .questions()
            .iter()
            .filter(|question| question.language_relation == EvalLanguageRelation::SameLanguage)
            .count(),
        7
    );
    assert_eq!(
        small_v2
            .questions()
            .iter()
            .filter(|question| question.language_relation == EvalLanguageRelation::CrossLanguage)
            .count(),
        5
    );
    assert_eq!(
        small_v2
            .questions()
            .iter()
            .filter(|question| question.language_relation == EvalLanguageRelation::MixedLanguage)
            .count(),
        3
    );
    assert_eq!(
        small_v2
            .questions()
            .iter()
            .filter(|question| question.evidence_hops.len() > 1)
            .count(),
        1
    );
}

#[test]
fn ranking_development_and_sealed_v3_suites_share_their_tier_haystacks() {
    let corpus = EvalCorpus::bundled().expect("bundled corpus");
    let small_dev = corpus
        .suite("small-ranking-dev")
        .expect("small ranking development suite");
    let medium_dev = corpus
        .suite("medium-ranking-dev")
        .expect("medium ranking development suite");
    let small_v3 = corpus
        .suite("small-holdout-v3")
        .expect("small sealed holdout v3 suite");
    let medium_v3 = corpus
        .suite("medium-holdout-v3")
        .expect("medium sealed holdout v3 suite");

    assert_eq!(small_dev.haystack(), "small");
    assert_eq!(medium_dev.haystack(), "medium");
    assert_eq!(small_v3.haystack(), "small");
    assert_eq!(medium_v3.haystack(), "medium");
    assert_eq!(small_dev.questions(), medium_dev.questions());
    assert_eq!(small_v3.questions(), medium_v3.questions());
    assert_eq!(small_dev.questions().len(), 6);
    assert_eq!(small_v3.questions().len(), 15);
    assert!(
        small_dev
            .questions()
            .iter()
            .all(|question| question.id.starts_with("ranking-dev-"))
    );
    assert!(
        small_v3
            .questions()
            .iter()
            .all(|question| question.id.starts_with("holdout-v3-"))
    );
    for ability in [
        EvalAbility::StaticState,
        EvalAbility::DynamicState,
        EvalAbility::Procedure,
        EvalAbility::Gotcha,
        EvalAbility::PremiseAwareness,
    ] {
        assert_eq!(
            small_v3
                .questions()
                .iter()
                .filter(|question| question.ability == ability)
                .count(),
            3
        );
    }
    assert_eq!(
        small_v3
            .questions()
            .iter()
            .filter(|question| question.language_relation == EvalLanguageRelation::SameLanguage)
            .count(),
        7
    );
    assert_eq!(
        small_v3
            .questions()
            .iter()
            .filter(|question| question.language_relation == EvalLanguageRelation::CrossLanguage)
            .count(),
        5
    );
    assert_eq!(
        small_v3
            .questions()
            .iter()
            .filter(|question| question.language_relation == EvalLanguageRelation::MixedLanguage)
            .count(),
        3
    );
    assert_eq!(
        small_v3
            .questions()
            .iter()
            .filter(|question| question.evidence_hops.len() > 1)
            .count(),
        1
    );
}

#[test]
fn medium_tier_scales_one_fixed_haystack_to_five_hundred_sessions() {
    let corpus = EvalCorpus::bundled().expect("bundled corpus");
    let medium_dev = corpus.suite("medium-dev").expect("medium dev suite");
    let medium_holdout = corpus
        .suite("medium-holdout")
        .expect("medium holdout suite");

    assert_eq!(medium_dev.haystack(), "medium");
    assert_eq!(medium_holdout.haystack(), "medium");
    assert_eq!(medium_dev.session_count(), 500);
    assert_eq!(medium_dev.record_count(), 1_506);
    assert_eq!(medium_dev.mutations().len(), 1_507);
    assert_eq!(medium_dev.mutations(), medium_holdout.mutations());
    assert_eq!(medium_dev.questions().len(), 15);
    assert_eq!(medium_holdout.questions().len(), 15);
}

#[tokio::test]
async fn bundled_corpus_splits_share_one_haystack_and_have_working_bounds() {
    let corpus = EvalCorpus::bundled().expect("bundled corpus");
    assert_eq!(corpus.manifest().schema_version, 2);
    assert_eq!(corpus.manifest().version, "1.4.0-holdout-v3");
    assert_eq!(
        corpus.suites().collect::<Vec<_>>(),
        [
            "medium-dev",
            "medium-holdout",
            "medium-holdout-v2",
            "medium-holdout-v3",
            "medium-ranking-dev",
            "small",
            "small-dev",
            "small-holdout",
            "small-holdout-v2",
            "small-holdout-v3",
            "small-ranking-dev",
            "smoke",
        ]
    );

    let suite = corpus.suite("smoke").expect("smoke suite");
    assert_eq!(suite.questions().len(), 15);
    assert_eq!(suite.session_count(), 7);
    assert_eq!(suite.record_count(), 27);

    let small_dev = corpus.suite("small-dev").expect("small dev suite");
    let small_holdout = corpus.suite("small-holdout").expect("small holdout suite");
    assert_eq!(small_dev.haystack(), "small");
    assert_eq!(small_holdout.haystack(), "small");
    assert_eq!(small_dev.session_count(), 100);
    assert_eq!(small_dev.record_count(), 306);
    assert_eq!(small_dev.mutations().len(), 307);
    assert_eq!(small_dev.mutations(), small_holdout.mutations());
    assert_eq!(small_dev.questions().len(), 15);
    assert_eq!(small_holdout.questions().len(), 15);
    assert!(
        small_dev
            .questions()
            .iter()
            .all(|question| !question.id.starts_with("holdout-"))
    );
    assert!(
        small_holdout
            .questions()
            .iter()
            .all(|question| question.id.starts_with("holdout-"))
    );
    let first_filler = small_dev
        .mutations()
        .into_iter()
        .find_map(|mutation| match mutation {
            MemoryMutation::Remember { record, .. }
                if record.id == "generated-small-000-record-0" =>
            {
                Some(record.text)
            }
            _ => None,
        })
        .expect("first deterministic filler");
    assert_eq!(
        first_filler,
        "Atlas test-fixture SQLite FTS5 fixture demonstrates MATCH queries containing punctuation and raw syntax."
    );

    let runner = EvalRunner::new(RunnerConfig {
        timeout: Duration::from_millis(500),
    });
    let oracle = runner
        .run("oracle", &suite, &OracleBackend::new(&suite))
        .await;
    assert_eq!(oracle.summary.completed, 15);
    assert!((oracle.summary.retrieval.recall_at_1 - (14.5 / 15.0)).abs() < f64::EPSILON);
    assert_eq!(oracle.summary.retrieval.recall_at_5, 1.0);
    assert_eq!(oracle.summary.retrieval.all_hops_rate, 1.0);
    assert_eq!(oracle.summary.retrieval.wrong_scope_hits, 0);
    assert_eq!(oracle.summary.retrieval.stale_hits, 0);
    assert_eq!(oracle.summary.retrieval.distractor_hits, 0);
    assert_eq!(
        oracle
            .summary
            .by_language_relation
            .get(&EvalLanguageRelation::SameLanguage)
            .expect("same-language metrics")
            .cases,
        8
    );
    assert_eq!(
        oracle
            .summary
            .by_language_relation
            .get(&EvalLanguageRelation::CrossLanguage)
            .expect("cross-language metrics")
            .cases,
        4
    );
    assert_eq!(
        oracle
            .summary
            .by_hop_class
            .get(&HopClass::MultiHop)
            .expect("multi-hop metrics")
            .cases,
        1
    );

    let small_oracle = runner
        .run("oracle", &small_dev, &OracleBackend::new(&small_dev))
        .await;
    assert_eq!(small_oracle.schema_version, 5);
    assert_eq!(small_oracle.suite, "small-dev");
    assert_eq!(small_oracle.haystack, "small");
    assert_eq!(small_oracle.summary.completed, 15);
    assert_eq!(small_oracle.summary.retrieval.recall_at_5, 1.0);
    assert_eq!(small_oracle.summary.retrieval.all_hops_rate, 1.0);

    let holdout_oracle = runner
        .run(
            "oracle",
            &small_holdout,
            &OracleBackend::new(&small_holdout),
        )
        .await;
    assert_eq!(holdout_oracle.summary.completed, 15);
    assert_eq!(holdout_oracle.summary.retrieval.recall_at_5, 1.0);
    assert_eq!(holdout_oracle.summary.retrieval.all_hops_rate, 1.0);
    assert_eq!(
        holdout_oracle
            .summary
            .by_language_relation
            .get(&EvalLanguageRelation::CrossLanguage)
            .expect("holdout cross-language metrics")
            .cases,
        8
    );

    let holdout_v2 = corpus
        .suite("small-holdout-v2")
        .expect("small holdout v2 suite");
    let holdout_v2_oracle = runner
        .run("oracle", &holdout_v2, &OracleBackend::new(&holdout_v2))
        .await;
    assert_eq!(holdout_v2_oracle.summary.completed, 15);
    assert_eq!(holdout_v2_oracle.summary.retrieval.recall_at_5, 1.0);
    assert_eq!(holdout_v2_oracle.summary.retrieval.all_hops_rate, 1.0);
    assert_eq!(holdout_v2_oracle.summary.retrieval.wrong_scope_hits, 0);
    assert_eq!(holdout_v2_oracle.summary.retrieval.stale_hits, 0);
    assert_eq!(holdout_v2_oracle.summary.retrieval.distractor_hits, 0);

    let holdout_v3 = corpus
        .suite("small-holdout-v3")
        .expect("small sealed holdout v3 suite");
    let holdout_v3_oracle = runner
        .run("oracle", &holdout_v3, &OracleBackend::new(&holdout_v3))
        .await;
    assert_eq!(holdout_v3_oracle.summary.completed, 15);
    assert_eq!(holdout_v3_oracle.summary.retrieval.recall_at_5, 1.0);
    assert_eq!(holdout_v3_oracle.summary.retrieval.all_hops_rate, 1.0);
    assert_eq!(holdout_v3_oracle.summary.retrieval.wrong_scope_hits, 0);
    assert_eq!(holdout_v3_oracle.summary.retrieval.stale_hits, 0);
    assert_eq!(holdout_v3_oracle.summary.retrieval.distractor_hits, 0);

    let no_recall = runner.run("no-recall", &suite, &NoRecallBackend).await;
    assert_eq!(no_recall.summary.completed, 15);
    assert_eq!(no_recall.summary.retrieval.recall_at_5, 0.0);
    assert_eq!(no_recall.summary.retrieval.all_hops_rate, 0.0);
}
