use std::path::PathBuf;
use std::sync::Arc;

use pi_plugin_memory_local::{
    EmbeddingPurpose, FastEmbedModelState, FastEmbedModelStore, LocalMemoryProvider,
    MemoryEvidence, MemoryKind, MemoryMutation, MemoryOrigin, MemoryRecord, MemoryScope,
    RecallQuery,
};

const CACHE_ENV: &str = "PI_MEMORY_EMBEDDING_CACHE";

#[tokio::test]
#[ignore = "requires an explicitly installed multilingual-e5-small model"]
async fn installed_model_drives_dense_backfill_and_cross_language_rescue() {
    let cache_dir = PathBuf::from(
        std::env::var_os(CACHE_ENV)
            .unwrap_or_else(|| panic!("set {CACHE_ENV} to the installed embedding cache")),
    );
    let store = FastEmbedModelStore::new(&cache_dir);
    assert!(matches!(
        store.status().state,
        FastEmbedModelState::Ready { .. }
    ));
    let reuse = store.install().await.unwrap();
    println!("install reuse: {} verified files", reuse.files);
    assert!(
        reuse.reused,
        "a ready installation must not use the network"
    );
    let embedder = store
        .embedder_if_ready()
        .unwrap()
        .expect("installed model must load");

    let query = embedder
        .embed(
            EmbeddingPurpose::Query,
            vec!["What response style does the user prefer?".to_string()],
        )
        .await
        .unwrap()
        .remove(0);
    let documents = embedder
        .embed(
            EmbeddingPurpose::Document,
            vec![
                "用户偏好简洁的中文回答，不喜欢冗长解释。".to_string(),
                "PostgreSQL 主数据库运行在东京区域，备份保留三十天。".to_string(),
                "用户每周六练习古典吉他。".to_string(),
            ],
        )
        .await
        .unwrap();
    assert_eq!(query.len(), 384);
    assert!(documents.iter().all(|embedding| embedding.len() == 384));
    assert_normalized(&query);
    for embedding in &documents {
        assert_normalized(embedding);
    }
    let preference_similarity = dot(&query, &documents[0]);
    let distractor_similarity = documents[1..]
        .iter()
        .map(|embedding| dot(&query, embedding))
        .fold(f32::NEG_INFINITY, f32::max);
    println!(
        "cross-language cosine: preference={preference_similarity:.6}, strongest distractor={distractor_similarity:.6}"
    );
    assert!(preference_similarity > distractor_similarity);

    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("memory.sqlite3");
    let lexical = LocalMemoryProvider::open(&database_path).unwrap();
    lexical
        .apply(vec![
            remember(
                "mutation-preference",
                record(
                    "preference",
                    MemoryKind::Preference,
                    "用户偏好简洁的中文回答，不喜欢冗长解释。",
                ),
            ),
            remember(
                "mutation-database",
                record(
                    "database",
                    MemoryKind::Fact,
                    "PostgreSQL 主数据库运行在东京区域，备份保留三十天。",
                ),
            ),
            remember(
                "mutation-music",
                record("music", MemoryKind::Fact, "用户每周六练习古典吉他。"),
            ),
        ])
        .await
        .unwrap();
    drop(lexical);

    let dense =
        Arc::new(LocalMemoryProvider::open_with_embedder(&database_path, embedder).unwrap());
    let before = dense.health().await.unwrap().embedding.unwrap();
    assert_eq!(before.dimensions, 384);
    assert_eq!(before.indexed_records, 0);
    assert_eq!(before.pending_records, 3);
    let backfill = dense.backfill_embeddings(32).await.unwrap();
    println!(
        "backfill: attempted={}, indexed={}, remaining={}",
        backfill.attempted, backfill.indexed, backfill.remaining
    );
    assert_eq!(backfill.attempted, 3);
    assert_eq!(backfill.indexed, 3);
    assert_eq!(backfill.remaining, 0);

    let direct = dense
        .recall(RecallQuery {
            text: "What response style does the user prefer?".to_string(),
            scopes: vec![MemoryScope::User],
            limit: 3,
        })
        .await
        .unwrap();
    println!(
        "direct hybrid hits: {:?}",
        direct
            .hits
            .iter()
            .map(|hit| (&hit.record.id, hit.score))
            .collect::<Vec<_>>()
    );
    assert_eq!(direct.hits.first().unwrap().record.id, "preference");

    let health = dense.health().await.unwrap();
    let embedding = health
        .embedding
        .expect("product runtime must activate dense recall");
    assert_eq!(embedding.indexed_records, 3);
    assert_eq!(embedding.pending_records, 0);
    let product = dense
        .recall(RecallQuery {
            text: "What response style does the user prefer?".to_string(),
            scopes: vec![MemoryScope::User],
            limit: 3,
        })
        .await
        .unwrap();
    assert_eq!(product.hits.first().unwrap().record.id, "preference");
}

fn remember(mutation_id: &str, record: MemoryRecord) -> MemoryMutation {
    MemoryMutation::Remember {
        mutation_id: mutation_id.to_string(),
        record,
    }
}

fn record(id: &str, kind: MemoryKind, text: &str) -> MemoryRecord {
    MemoryRecord {
        id: id.to_string(),
        scope: MemoryScope::User,
        kind,
        text: text.to_string(),
        origin: MemoryOrigin {
            session_id: "dense-model-smoke".to_string(),
            entry_id: Some(format!("entry-{id}")),
            tool_call_id: None,
        },
        evidence: MemoryEvidence {
            note: "real embedding smoke test".to_string(),
        },
        recorded_at_ms: 1,
        supersedes: None,
    }
}

fn dot(left: &[f32], right: &[f32]) -> f32 {
    left.iter()
        .zip(right)
        .map(|(left, right)| left * right)
        .sum()
}

fn assert_normalized(embedding: &[f32]) {
    let norm = dot(embedding, embedding).sqrt();
    assert!((norm - 1.0).abs() < 1e-3, "embedding norm was {norm}");
}
