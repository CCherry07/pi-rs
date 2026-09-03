use crate::runtime::{InstalledMemoryEmbedding, MemoryEmbeddingRuntimeStatus};
use crate::{FastEmbedModelState, MemoryEmbeddingBackfillReceipt, MemoryHealth, MemoryHit};

const MAX_NOTICE_BYTES: usize = 32 * 1024;

pub(super) fn health(health: &MemoryHealth, model: &MemoryEmbeddingRuntimeStatus) -> String {
    let status = if health.is_healthy() {
        "healthy"
    } else {
        "degraded"
    };
    let recovery = health
        .recovered_from
        .as_ref()
        .map_or_else(String::new, |path| {
            format!(
                "\nRecovered database: {}\nRun `/memory-local-rebuild` to repopulate all sessions.",
                path.display()
            )
        });
    let embedding = health.embedding.as_ref().map_or_else(
        || "Dense index: disabled".to_string(),
        |embedding| {
            format!(
                "Dense index: {}@{} ({}d {}, {} indexed / {} pending)",
                embedding.model,
                embedding.revision,
                embedding.dimensions,
                embedding.distance_metric,
                embedding.indexed_records,
                embedding.pending_records,
            )
        },
    );
    format!(
        "Memory status: {status}\n\nDatabase: {}\nSchema: {}\nIntegrity: {}\nSize: {} bytes\nMutations: {}\nRecords: {} active / {} total\nTombstones: {}\nIndexed sessions: {}\nIndexed entries: {}\nSQLite vec: {}\n{}{recovery}\n\n{}",
        health.database_path.display(),
        health.schema_version,
        health.integrity_check,
        health.database_bytes,
        health.mutations,
        health.active_records,
        health.records,
        health.tombstones,
        health.sessions,
        health.session_entries,
        health.vector_extension_version,
        embedding,
        model_status(model),
    )
}

pub(super) fn model_status(status: &MemoryEmbeddingRuntimeStatus) -> String {
    let state = match &status.model.state {
        FastEmbedModelState::Missing => "not installed",
        FastEmbedModelState::Ready { .. } => "ready",
        FastEmbedModelState::Invalid { .. } => "invalid",
    };
    let installed = match &status.model.state {
        FastEmbedModelState::Ready { installed_at_ms } => {
            format!("\nInstalled at: {installed_at_ms} ms since Unix epoch")
        }
        FastEmbedModelState::Invalid { message } => format!("\nAsset problem: {message}"),
        FastEmbedModelState::Missing => String::new(),
    };
    let retrieval = if status.dense_active {
        "active (BM25 + dense RRF)".to_string()
    } else if let Some(error) = &status.runtime_error {
        format!("lexical fallback\nRuntime problem: {error}")
    } else {
        "lexical fallback".to_string()
    };
    format!(
        "Embedding model: {state}\n\nModel: {}\nRevision: {}\nDimensions: {}\nCache: {}\nExpected assets: {}\nDense retrieval: {retrieval}{installed}",
        status.model.descriptor.model,
        status.model.descriptor.revision,
        status.model.descriptor.dimensions,
        status.model.cache_dir.display(),
        format_bytes(status.model.expected_download_bytes),
    )
}

pub(super) fn model_install(installed: &InstalledMemoryEmbedding) -> String {
    let install = &installed.install;
    let backfill = &installed.backfill;
    let action = if install.reused {
        "Embedding model already verified"
    } else {
        "Embedding model installed"
    };
    format!(
        "{action}\n\nModel: {}\nRevision: {}\nAssets: {} files / {}\nCache: {}\nBackfill: {} indexed / {} attempted / {} remaining\nDense retrieval: active (BM25 + dense RRF)",
        install.descriptor.model,
        install.descriptor.revision,
        install.files,
        format_bytes(install.bytes),
        install.cache_dir.display(),
        backfill.indexed,
        backfill.attempted,
        backfill.remaining,
    )
}

pub(super) fn optional_backfill(backfill: Option<&MemoryEmbeddingBackfillReceipt>) -> String {
    backfill.map_or_else(String::new, |backfill| {
        format!(
            "\nDense vectors: {} indexed, {} remaining",
            backfill.indexed, backfill.remaining
        )
    })
}

pub(super) fn hits(hits: &[MemoryHit]) -> String {
    if hits.is_empty() {
        return "No active memories matched the current user, project, and session scopes."
            .to_string();
    }
    let mut output = format!("Active memories ({})\n", hits.len());
    for hit in hits {
        let entry = hit
            .record
            .origin
            .entry_id
            .as_deref()
            .map_or_else(String::new, |entry| format!(" entry={entry}"));
        let block = format!(
            "\n- id={} scope={} kind={}\n  origin={}{}\n  evidence={}\n  {}\n",
            hit.record.id,
            hit.record.scope.key(),
            hit.record.kind.as_str(),
            hit.record.origin.session_id,
            entry,
            hit.record.evidence.note.replace('\n', " "),
            hit.record.text.replace('\n', " "),
        );
        if output.len() + block.len() > MAX_NOTICE_BYTES {
            output.push_str("\n… output truncated");
            break;
        }
        output.push_str(&block);
    }
    output
}

fn format_bytes(bytes: u64) -> String {
    const MEBIBYTE: f64 = 1024.0 * 1024.0;
    format!("{:.1} MiB", bytes as f64 / MEBIBYTE)
}
