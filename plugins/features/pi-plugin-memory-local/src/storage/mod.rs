mod sqlite_vec;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::{
    Connection, ErrorCode, OptionalExtension, Transaction, TransactionBehavior, params,
    params_from_iter,
};

use crate::{
    ApplyReceipt, EmbeddingDescriptor, EmbeddingPurpose, MemoryEmbedder, MemoryError, MemoryHit,
    MemoryMutation, MemoryRecord, MemoryScope, RecallQuery, RecallResult, SessionIndexDocument,
    SessionSearchHit, SessionSearchQuery,
};

const SCHEMA_VERSION: i64 = 2;
const DENSE_VARIANT_RRF_K: f64 = 60.0;

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
CREATE TABLE IF NOT EXISTS memory_mutations (
    id TEXT PRIMARY KEY,
    payload_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS memory_records (
    id TEXT PRIMARY KEY,
    mutation_id TEXT NOT NULL UNIQUE,
    scope_key TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    text TEXT NOT NULL,
    recorded_at_ms INTEGER NOT NULL,
    supersedes TEXT
);
CREATE INDEX IF NOT EXISTS memory_records_scope_time
    ON memory_records(scope_key, recorded_at_ms DESC);
CREATE INDEX IF NOT EXISTS memory_records_supersedes
    ON memory_records(supersedes);
CREATE TABLE IF NOT EXISTS memory_tombstones (
    mutation_id TEXT PRIMARY KEY,
    target_id TEXT NOT NULL,
    reason TEXT NOT NULL,
    recorded_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS memory_tombstones_target
    ON memory_tombstones(target_id);
CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(
    record_id UNINDEXED,
    text,
    tokenize = 'unicode61 remove_diacritics 2'
);
CREATE TABLE IF NOT EXISTS memory_embedding_index (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    model TEXT NOT NULL,
    revision TEXT NOT NULL,
    dimensions INTEGER NOT NULL,
    distance_metric TEXT NOT NULL,
    extension_version TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS session_entries (
    document_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    project_key TEXT NOT NULL,
    entry_id TEXT NOT NULL,
    role TEXT NOT NULL,
    text TEXT NOT NULL,
    timestamp_ms INTEGER NOT NULL,
    UNIQUE(session_id, entry_id)
);
CREATE INDEX IF NOT EXISTS session_entries_project_time
    ON session_entries(project_key, timestamp_ms DESC);
CREATE INDEX IF NOT EXISTS session_entries_session_time
    ON session_entries(session_id, timestamp_ms DESC);
CREATE VIRTUAL TABLE IF NOT EXISTS session_fts USING fts5(
    document_id UNINDEXED,
    text,
    tokenize = 'unicode61 remove_diacritics 2'
);
"#;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryRebuildBatch {
    pub mutations: Vec<MemoryMutation>,
    pub sessions: Vec<SessionIndexDocument>,
    pub source_files: usize,
    pub skipped_files: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemoryRebuildReceipt {
    pub source_files: usize,
    pub skipped_files: usize,
    pub mutations: usize,
    pub duplicate_mutations: usize,
    pub sessions: usize,
    pub session_entries: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryHealth {
    pub database_path: PathBuf,
    pub schema_version: i64,
    pub integrity_check: String,
    pub database_bytes: u64,
    pub mutations: usize,
    pub records: usize,
    pub active_records: usize,
    pub tombstones: usize,
    pub sessions: usize,
    pub session_entries: usize,
    pub vector_extension_version: String,
    pub embedding: Option<MemoryEmbeddingHealth>,
    pub recovered_from: Option<PathBuf>,
}

impl MemoryHealth {
    pub fn is_healthy(&self) -> bool {
        self.schema_version == SCHEMA_VERSION && self.integrity_check == "ok"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryEmbeddingHealth {
    pub model: String,
    pub revision: String,
    pub dimensions: usize,
    pub distance_metric: String,
    pub indexed_records: usize,
    pub pending_records: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemoryEmbeddingBackfillReceipt {
    pub attempted: usize,
    pub indexed: usize,
    pub remaining: usize,
}

/// Final recall plus the rank-ordered source candidates used by the concrete
/// SQLite Adapter.
///
/// This is an evaluation diagnostic, not part of the product plugin Interface.
/// Candidate identities are captured before the final ranking, confidence,
/// cutoff, and diversity policy runs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SqliteRecallCandidates {
    pub result: RecallResult,
    pub sparse_record_ids: Vec<String>,
    pub dense_record_ids: Vec<String>,
    pub ranking_stages: Option<SqliteRankingStages>,
}

/// Candidate identities at the product hybrid ranker's policy boundaries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SqliteRankingStages {
    pub protected_core_record_ids: Vec<String>,
    pub gate_eligible_record_ids: Vec<String>,
    pub pre_cutoff_record_ids: Vec<String>,
}

#[derive(Clone)]
pub struct LocalMemoryProvider {
    path: PathBuf,
    ranking: SqliteRecallRanking,
    embedder: Option<Arc<dyn MemoryEmbedder>>,
    recovered_from: Arc<Mutex<Option<PathBuf>>>,
}

impl std::fmt::Debug for LocalMemoryProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalMemoryProvider")
            .field("path", &self.path)
            .field("ranking", &self.ranking)
            .field(
                "embedding",
                &self.embedder.as_ref().map(|embedder| embedder.descriptor()),
            )
            .field("recovered_from", &self.recovered_from)
            .finish()
    }
}

/// Candidate-ranking strategy used by the concrete SQLite Adapter.
///
/// `Bm25` is retained as an evaluation control. Product construction uses the
/// bounded lightweight `Hybrid` strategy by default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SqliteRecallRanking {
    /// Return the direct FTS5/BM25 order.
    Bm25,
    /// Re-rank sparse candidates with lexical structure and diversity signals.
    #[default]
    Hybrid,
    /// Historical equal-weight RRF control without confidence or diversity.
    SparseDenseRawRrf,
    /// Protected lexical ranking with confidence-gated dense rescue; RRF is a
    /// bounded agreement signal rather than the final score.
    SparseDenseRrf,
}

impl SqliteRecallRanking {
    const fn uses_dense(self) -> bool {
        matches!(self, Self::SparseDenseRawRrf | Self::SparseDenseRrf)
    }
}

impl LocalMemoryProvider {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, MemoryError> {
        Self::open_internal(path.into(), SqliteRecallRanking::default(), None)
    }

    /// Opens a provider with an explicit concrete-Adapter ranking strategy.
    pub fn open_with_ranking(
        path: impl Into<PathBuf>,
        ranking: SqliteRecallRanking,
    ) -> Result<Self, MemoryError> {
        let path = path.into();
        if ranking.uses_dense() {
            return Err(MemoryError::Initialize {
                path: path.display().to_string(),
                message: "sparse/dense ranking requires an embedding adapter".to_string(),
            });
        }
        Self::open_internal(path, ranking, None)
    }

    /// Opens the concrete SQLite Adapter with a derived dense index.
    ///
    /// Embedding failures during ordinary writes and queries degrade to sparse
    /// recall; callers can inspect or repair pending vectors through concrete
    /// maintenance methods.
    pub fn open_with_embedder(
        path: impl Into<PathBuf>,
        embedder: Arc<dyn MemoryEmbedder>,
    ) -> Result<Self, MemoryError> {
        Self::open_with_embedder_and_ranking(path, embedder, SqliteRecallRanking::SparseDenseRrf)
    }

    /// Opens a dense Adapter with an explicit evaluation ranking strategy.
    ///
    /// Product construction should use [`Self::open_with_embedder`]. This
    /// concrete seam exists so evaluations can compare the current policy with
    /// historical controls without widening product plugin policy.
    pub fn open_with_embedder_and_ranking(
        path: impl Into<PathBuf>,
        embedder: Arc<dyn MemoryEmbedder>,
        ranking: SqliteRecallRanking,
    ) -> Result<Self, MemoryError> {
        let path = path.into();
        if !ranking.uses_dense() {
            return Err(MemoryError::Initialize {
                path: path.display().to_string(),
                message: "embedding adapter requires a sparse/dense ranking strategy".to_string(),
            });
        }
        Self::open_internal(path, ranking, Some(embedder))
    }

    /// Runs recall while retaining the concrete Adapter's source candidates.
    ///
    /// Product callers should use [`Self::recall`]. This method is intentionally
    /// separate so evaluation diagnostics do not widen the product plugin
    /// Interface or expose gold metadata to retrieval.
    pub async fn recall_with_candidates(
        &self,
        query: RecallQuery,
    ) -> Result<SqliteRecallCandidates, MemoryError> {
        let ranking = self.ranking;
        let query_embeddings = if ranking.uses_dense() && !query.text.trim().is_empty() {
            self.query_embeddings(&query.text, ranking).await
        } else {
            None
        };
        self.run(move |connection| {
            recall_with_candidates(connection, query, ranking, query_embeddings.as_deref())
        })
        .await
    }

    fn open_internal(
        path: PathBuf,
        ranking: SqliteRecallRanking,
        embedder: Option<Arc<dyn MemoryEmbedder>>,
    ) -> Result<Self, MemoryError> {
        sqlite_vec::register().map_err(|error| MemoryError::Initialize {
            path: path.display().to_string(),
            message: error.to_string(),
        })?;
        if let Some(embedder) = &embedder {
            embedder
                .descriptor()
                .validate()
                .map_err(|error| MemoryError::Initialize {
                    path: path.display().to_string(),
                    message: error.to_string(),
                })?;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| MemoryError::Initialize {
                path: path.display().to_string(),
                message: error.to_string(),
            })?;
            restrict_directory_permissions(parent).map_err(|error| MemoryError::Initialize {
                path: parent.display().to_string(),
                message: error.to_string(),
            })?;
        }
        let descriptor = embedder.as_ref().map(|embedder| embedder.descriptor());
        let recovered_from = match initialize_database(&path, descriptor) {
            Ok(()) => None,
            Err(error) if is_corruption(&error) && path.is_file() => {
                let backup = quarantine_corrupt_database(&path).map_err(|quarantine_error| {
                    MemoryError::Initialize {
                        path: path.display().to_string(),
                        message: format!(
                            "database is corrupt ({error}); failed to preserve it: {quarantine_error}"
                        ),
                    }
                })?;
                initialize_database(&path, descriptor).map_err(|retry_error| MemoryError::Initialize {
                    path: path.display().to_string(),
                    message: format!(
                        "database was preserved at {}; replacement initialization failed: {retry_error}",
                        backup.display()
                    ),
                })?;
                Some(backup)
            }
            Err(error) => {
                return Err(MemoryError::Initialize {
                    path: path.display().to_string(),
                    message: error.to_string(),
                });
            }
        };
        restrict_database_permissions(&path).map_err(|error| MemoryError::Initialize {
            path: path.display().to_string(),
            message: error.to_string(),
        })?;
        Ok(Self {
            path,
            ranking,
            embedder,
            recovered_from: Arc::new(Mutex::new(recovered_from)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn dense_active(&self) -> bool {
        self.ranking.uses_dense() && self.embedder.is_some()
    }

    pub async fn health(&self) -> Result<MemoryHealth, MemoryError> {
        let path = self.path.clone();
        let recovered_from = self
            .recovered_from
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        self.run(move |connection| health(connection, path, recovered_from))
            .await
    }

    /// Replaces every derived row inside one immediate transaction.
    ///
    /// The source closure runs only after SQLite has acquired the write lock.
    /// Durable journal appends may continue concurrently, but their subsequent
    /// provider writes wait until this rebuild commits and therefore cannot be
    /// erased by the replacement.
    pub async fn rebuild_with<F>(&self, source: F) -> Result<MemoryRebuildReceipt, MemoryError>
    where
        F: FnOnce() -> Result<MemoryRebuildBatch, MemoryError> + Send + 'static,
    {
        let path = self.path.clone();
        let recovered_from = Arc::clone(&self.recovered_from);
        tokio::task::spawn_blocking(move || {
            let mut connection =
                open_connection(&path).map_err(|error| MemoryError::Provider(error.to_string()))?;
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(provider_error)?;
            let batch = source()?;
            let receipt = rebuild(&transaction, batch)?;
            transaction.commit().map_err(provider_error)?;
            restrict_database_permissions(&path)
                .map_err(|error| MemoryError::Provider(error.to_string()))?;
            *recovered_from
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            Ok(receipt)
        })
        .await
        .map_err(|error| MemoryError::Worker(error.to_string()))?
    }

    async fn run<T, F>(&self, operation: F) -> Result<T, MemoryError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, MemoryError> + Send + 'static,
    {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let mut connection =
                open_connection(&path).map_err(|error| MemoryError::Provider(error.to_string()))?;
            let result = operation(&mut connection);
            if result.is_ok() {
                restrict_database_permissions(&path)
                    .map_err(|error| MemoryError::Provider(error.to_string()))?;
            }
            result
        })
        .await
        .map_err(|error| MemoryError::Worker(error.to_string()))?
    }

    async fn query_embeddings(
        &self,
        text: &str,
        ranking: SqliteRecallRanking,
    ) -> Option<Vec<Vec<u8>>> {
        let embedder = self.embedder.as_ref()?;
        let texts = if ranking == SqliteRecallRanking::SparseDenseRrf {
            crate::ranking::semantic_query_variants(text)
        } else {
            vec![text.to_string()]
        };
        let expected_count = texts.len();
        let embeddings = embedder.embed(EmbeddingPurpose::Query, texts).await.ok()?;
        crate::embedding::validate_embeddings(embedder.descriptor(), expected_count, embeddings)
            .ok()
    }

    async fn mutation_embeddings(&self, mutations: &[MemoryMutation]) -> HashMap<String, Vec<u8>> {
        let Some(embedder) = &self.embedder else {
            return HashMap::new();
        };
        let records = mutations
            .iter()
            .filter_map(|mutation| match mutation {
                MemoryMutation::Remember { record, .. } => Some(record),
                MemoryMutation::Forget { .. } => None,
            })
            .collect::<Vec<_>>();
        if records.is_empty() {
            return HashMap::new();
        }
        let texts = records
            .iter()
            .map(|record| record.text.clone())
            .collect::<Vec<_>>();
        let Ok(embeddings) = embedder.embed(EmbeddingPurpose::Document, texts).await else {
            return HashMap::new();
        };
        let Ok(embeddings) =
            crate::embedding::validate_embeddings(embedder.descriptor(), records.len(), embeddings)
        else {
            return HashMap::new();
        };
        records
            .into_iter()
            .map(|record| record.id.clone())
            .zip(embeddings)
            .collect()
    }

    /// Populate a bounded batch of active records missing from the dense
    /// index. Model work happens outside SQLite transactions.
    pub async fn backfill_embeddings(
        &self,
        limit: usize,
    ) -> Result<MemoryEmbeddingBackfillReceipt, MemoryError> {
        let embedder = self.embedder.as_ref().ok_or_else(|| {
            MemoryError::Maintenance(
                "embedding backfill is unavailable without an embedding adapter".to_string(),
            )
        })?;
        if limit == 0 {
            let health = self.health().await?;
            return Ok(MemoryEmbeddingBackfillReceipt {
                remaining: health.embedding.map_or(0, |status| status.pending_records),
                ..MemoryEmbeddingBackfillReceipt::default()
            });
        }
        let records = self
            .run(move |connection| missing_embedding_records(connection, limit))
            .await?;
        if records.is_empty() {
            return Ok(MemoryEmbeddingBackfillReceipt::default());
        }
        let embeddings = embedder
            .embed(
                EmbeddingPurpose::Document,
                records.iter().map(|record| record.text.clone()).collect(),
            )
            .await
            .map_err(|error| MemoryError::Maintenance(error.to_string()))?;
        let embeddings =
            crate::embedding::validate_embeddings(embedder.descriptor(), records.len(), embeddings)
                .map_err(|error| MemoryError::Maintenance(error.to_string()))?;
        let attempted = records.len();
        let indexed = self
            .run(move |connection| index_embeddings(connection, records, embeddings))
            .await?;
        let remaining = self
            .health()
            .await?
            .embedding
            .map_or(0, |status| status.pending_records);
        Ok(MemoryEmbeddingBackfillReceipt {
            attempted,
            indexed,
            remaining,
        })
    }

    pub async fn recall(&self, query: RecallQuery) -> Result<RecallResult, MemoryError> {
        let ranking = self.ranking;
        let query_embeddings = if ranking.uses_dense() && !query.text.trim().is_empty() {
            self.query_embeddings(&query.text, ranking).await
        } else {
            None
        };
        self.run(move |connection| recall(connection, query, ranking, query_embeddings.as_deref()))
            .await
    }

    pub async fn apply(&self, mutations: Vec<MemoryMutation>) -> Result<ApplyReceipt, MemoryError> {
        for mutation in &mutations {
            mutation.validate()?;
        }
        let embeddings = self.mutation_embeddings(&mutations).await;
        self.run(move |connection| apply(connection, mutations, &embeddings))
            .await
    }

    pub async fn replace_session(&self, document: SessionIndexDocument) -> Result<(), MemoryError> {
        self.run(move |connection| replace_session(connection, document))
            .await
    }

    pub async fn search_sessions(
        &self,
        query: SessionSearchQuery,
    ) -> Result<Vec<SessionSearchHit>, MemoryError> {
        self.run(move |connection| search_sessions(connection, query))
            .await
    }
}

fn open_connection(path: &Path) -> rusqlite::Result<Connection> {
    let connection = Connection::open(path)?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    connection.execute_batch("PRAGMA foreign_keys = ON;")?;
    Ok(connection)
}

#[derive(Debug, thiserror::Error)]
enum DatabaseInitializationError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("unsupported memory database schema {found}; this build supports {supported}")]
    UnsupportedSchema { found: i64, supported: i64 },
}

fn initialize_database(
    path: &Path,
    descriptor: Option<&EmbeddingDescriptor>,
) -> Result<(), DatabaseInitializationError> {
    let connection = open_connection(path)?;
    let version = connection.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?;
    if version > SCHEMA_VERSION {
        return Err(DatabaseInitializationError::UnsupportedSchema {
            found: version,
            supported: SCHEMA_VERSION,
        });
    }
    connection.execute_batch(SCHEMA)?;
    if let Some(descriptor) = descriptor {
        initialize_vector_index(&connection, descriptor)?;
    }
    connection.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

fn initialize_vector_index(
    connection: &Connection,
    descriptor: &EmbeddingDescriptor,
) -> rusqlite::Result<()> {
    let extension_version =
        connection.query_row("SELECT vec_version()", [], |row| row.get::<_, String>(0))?;
    let current = connection
        .query_row(
            "SELECT model, revision, dimensions, distance_metric
             FROM memory_embedding_index WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    let vector_table_exists = table_exists(connection, "memory_vec")?;
    let dimensions = i64::try_from(descriptor.dimensions)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    let matches = current
        .as_ref()
        .is_some_and(|(model, revision, found_dimensions, metric)| {
            model == &descriptor.model
                && revision == &descriptor.revision
                && *found_dimensions == dimensions
                && metric == "cosine"
                && vector_table_exists
        });
    if matches {
        return Ok(());
    }

    let transaction = connection.unchecked_transaction()?;
    transaction.execute_batch("DROP TABLE IF EXISTS memory_vec;")?;
    transaction.execute_batch(&format!(
        "CREATE VIRTUAL TABLE memory_vec USING vec0(
             record_id TEXT PRIMARY KEY,
             embedding FLOAT[{}] distance_metric=cosine,
             scope_key TEXT
         );",
        descriptor.dimensions
    ))?;
    transaction.execute("DELETE FROM memory_embedding_index", [])?;
    transaction.execute(
        "INSERT INTO memory_embedding_index(
             singleton, model, revision, dimensions, distance_metric, extension_version
         ) VALUES (1, ?1, ?2, ?3, 'cosine', ?4)",
        params![
            descriptor.model,
            descriptor.revision,
            dimensions,
            extension_version
        ],
    )?;
    transaction.commit()
}

fn table_exists(connection: &Connection, table: &str) -> rusqlite::Result<bool> {
    connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![table],
            |_| Ok(()),
        )
        .optional()
        .map(|value| value.is_some())
}

fn is_corruption(error: &DatabaseInitializationError) -> bool {
    matches!(
        error,
        DatabaseInitializationError::Sqlite(rusqlite::Error::SqliteFailure(failure, _))
            if matches!(
                failure.code,
                ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase
            )
    )
}

fn quarantine_corrupt_database(path: &Path) -> std::io::Result<PathBuf> {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    let file_name = path
        .file_name()
        .map_or_else(|| OsString::from("memory.sqlite3"), OsString::from);
    let mut backup_name = file_name;
    backup_name.push(format!(".corrupt-{timestamp}-{}", std::process::id()));
    let backup = path.with_file_name(backup_name);
    std::fs::rename(path, &backup)?;
    for suffix in ["-wal", "-shm"] {
        let source = sqlite_sidecar(path, suffix);
        if source.exists() {
            std::fs::rename(source, sqlite_sidecar(&backup, suffix))?;
        }
    }
    Ok(backup)
}

fn sqlite_sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn restrict_database_permissions(path: &Path) -> std::io::Result<()> {
    for candidate in [
        path.to_path_buf(),
        sqlite_sidecar(path, "-wal"),
        sqlite_sidecar(path, "-shm"),
    ] {
        if candidate.exists() {
            restrict_file_permissions(&candidate)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn restrict_directory_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn restrict_directory_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn restrict_file_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_file_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

fn apply(
    connection: &mut Connection,
    mutations: Vec<MemoryMutation>,
    embeddings: &HashMap<String, Vec<u8>>,
) -> Result<ApplyReceipt, MemoryError> {
    let transaction = connection.transaction().map_err(provider_error)?;
    let receipt = apply_in_transaction(&transaction, mutations, embeddings)?;
    transaction.commit().map_err(provider_error)?;
    Ok(receipt)
}

fn apply_in_transaction(
    transaction: &Transaction<'_>,
    mutations: Vec<MemoryMutation>,
    embeddings: &HashMap<String, Vec<u8>>,
) -> Result<ApplyReceipt, MemoryError> {
    let mut receipt = ApplyReceipt::default();
    for mutation in mutations {
        let payload = serde_json::to_string(&mutation).map_err(provider_error)?;
        let inserted = transaction
            .execute(
                "INSERT OR IGNORE INTO memory_mutations(id, payload_json) VALUES (?1, ?2)",
                params![mutation.id(), payload],
            )
            .map_err(provider_error)?;
        if inserted == 0 {
            let existing: String = transaction
                .query_row(
                    "SELECT payload_json FROM memory_mutations WHERE id = ?1",
                    params![mutation.id()],
                    |row| row.get(0),
                )
                .map_err(provider_error)?;
            if existing != payload {
                return Err(MemoryError::Provider(format!(
                    "mutation id {} has conflicting journal payloads",
                    mutation.id()
                )));
            }
            receipt.duplicates += 1;
        } else {
            match &mutation {
                MemoryMutation::Remember {
                    mutation_id,
                    record,
                } => insert_record(transaction, mutation_id, record)?,
                MemoryMutation::Forget {
                    mutation_id,
                    target_id,
                    reason,
                    recorded_at_ms,
                    ..
                } => {
                    transaction
                    .execute(
                        "INSERT INTO memory_tombstones(mutation_id, target_id, reason, recorded_at_ms) \
                         VALUES (?1, ?2, ?3, ?4)",
                        params![mutation_id, target_id, reason, recorded_at_ms],
                    )
                    .map_err(provider_error)?;
                }
            }
            receipt.applied += 1;
        }
        sync_vector_mutation(transaction, &mutation, embeddings)?;
    }
    Ok(receipt)
}

fn rebuild(
    transaction: &Transaction<'_>,
    batch: MemoryRebuildBatch,
) -> Result<MemoryRebuildReceipt, MemoryError> {
    for mutation in &batch.mutations {
        mutation.validate()?;
    }
    transaction
        .execute_batch(
            "DELETE FROM memory_fts;
             DELETE FROM memory_tombstones;
             DELETE FROM memory_records;
             DELETE FROM memory_mutations;
             DELETE FROM session_fts;
             DELETE FROM session_entries;",
        )
        .map_err(provider_error)?;
    if table_exists(transaction, "memory_vec").map_err(provider_error)? {
        transaction
            .execute("DELETE FROM memory_vec", [])
            .map_err(provider_error)?;
    }
    let mutation_receipt = apply_in_transaction(transaction, batch.mutations, &HashMap::new())?;
    let mut receipt = MemoryRebuildReceipt {
        source_files: batch.source_files,
        skipped_files: batch.skipped_files,
        mutations: mutation_receipt.applied,
        duplicate_mutations: mutation_receipt.duplicates,
        ..MemoryRebuildReceipt::default()
    };
    for document in batch.sessions {
        receipt.sessions += 1;
        receipt.session_entries += document
            .entries
            .iter()
            .filter(|entry| !entry.text.trim().is_empty())
            .count();
        replace_session_in_transaction(transaction, document)?;
    }
    Ok(receipt)
}

fn insert_record(
    transaction: &Transaction<'_>,
    mutation_id: &str,
    record: &MemoryRecord,
) -> Result<(), MemoryError> {
    let payload = serde_json::to_string(record).map_err(provider_error)?;
    transaction
        .execute(
            "INSERT INTO memory_records(\
                 id, mutation_id, scope_key, payload_json, text, recorded_at_ms, supersedes\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                record.id,
                mutation_id,
                record.scope.key(),
                payload,
                record.text,
                record.recorded_at_ms,
                record.supersedes,
            ],
        )
        .map_err(provider_error)?;
    transaction
        .execute(
            "INSERT INTO memory_fts(record_id, text) VALUES (?1, ?2)",
            params![record.id, record.text],
        )
        .map_err(provider_error)?;
    Ok(())
}

fn sync_vector_mutation(
    transaction: &Transaction<'_>,
    mutation: &MemoryMutation,
    embeddings: &HashMap<String, Vec<u8>>,
) -> Result<(), MemoryError> {
    if !table_exists(transaction, "memory_vec").map_err(provider_error)? {
        return Ok(());
    }
    match mutation {
        MemoryMutation::Remember { record, .. } => {
            if let Some(target) = &record.supersedes {
                transaction
                    .execute(
                        "DELETE FROM memory_vec WHERE record_id = ?1",
                        params![target],
                    )
                    .map_err(provider_error)?;
            }
            if let Some(embedding) = embeddings.get(&record.id) {
                sync_record_vector(transaction, record, embedding)?;
            }
        }
        MemoryMutation::Forget { target_id, .. } => {
            transaction
                .execute(
                    "DELETE FROM memory_vec WHERE record_id = ?1",
                    params![target_id],
                )
                .map_err(provider_error)?;
        }
    }
    Ok(())
}

fn sync_record_vector(
    transaction: &Transaction<'_>,
    record: &MemoryRecord,
    embedding: &[u8],
) -> Result<bool, MemoryError> {
    transaction
        .execute(
            "DELETE FROM memory_vec WHERE record_id = ?1",
            params![record.id],
        )
        .map_err(provider_error)?;
    if !record_is_active(transaction, &record.id)? {
        return Ok(false);
    }
    transaction
        .execute(
            "INSERT INTO memory_vec(record_id, embedding, scope_key) VALUES (?1, ?2, ?3)",
            params![record.id, embedding, record.scope.key()],
        )
        .map_err(provider_error)?;
    Ok(true)
}

fn record_is_active(connection: &Connection, record_id: &str) -> Result<bool, MemoryError> {
    connection
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM memory_records r
                 WHERE r.id = ?1
                   AND NOT EXISTS (
                       SELECT 1 FROM memory_tombstones t WHERE t.target_id = r.id
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM memory_records n WHERE n.supersedes = r.id
                   )
             )",
            params![record_id],
            |row| row.get::<_, bool>(0),
        )
        .map_err(provider_error)
}

fn missing_embedding_records(
    connection: &Connection,
    limit: usize,
) -> Result<Vec<MemoryRecord>, MemoryError> {
    if !table_exists(connection, "memory_vec").map_err(provider_error)? {
        return Ok(Vec::new());
    }
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let mut statement = connection
        .prepare(
            "SELECT r.payload_json FROM memory_records r
             WHERE NOT EXISTS (
                 SELECT 1 FROM memory_tombstones t WHERE t.target_id = r.id
             ) AND NOT EXISTS (
                 SELECT 1 FROM memory_records n WHERE n.supersedes = r.id
             ) AND NOT EXISTS (
                 SELECT 1 FROM memory_vec v WHERE v.record_id = r.id
             )
             ORDER BY r.recorded_at_ms, r.id
             LIMIT ?1",
        )
        .map_err(provider_error)?;
    let rows = statement
        .query_map(params![limit], |row| row.get::<_, String>(0))
        .map_err(provider_error)?;
    rows.map(|row| {
        let payload = row.map_err(provider_error)?;
        serde_json::from_str(&payload).map_err(provider_error)
    })
    .collect()
}

fn index_embeddings(
    connection: &mut Connection,
    records: Vec<MemoryRecord>,
    embeddings: Vec<Vec<u8>>,
) -> Result<usize, MemoryError> {
    let transaction = connection.transaction().map_err(provider_error)?;
    let mut indexed = 0;
    for (record, embedding) in records.iter().zip(&embeddings) {
        indexed += usize::from(sync_record_vector(&transaction, record, embedding)?);
    }
    transaction.commit().map_err(provider_error)?;
    Ok(indexed)
}

fn health(
    connection: &Connection,
    database_path: PathBuf,
    recovered_from: Option<PathBuf>,
) -> Result<MemoryHealth, MemoryError> {
    let schema_version = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(provider_error)?;
    let integrity_check = connection
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .map_err(provider_error)?;
    let count = |table: &str| -> Result<usize, MemoryError> {
        let value = connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(provider_error)?;
        usize::try_from(value)
            .map_err(|_| MemoryError::Provider(format!("invalid row count for {table}: {value}")))
    };
    let active_records = connection
        .query_row(
            "SELECT count(*) FROM memory_records r
             WHERE NOT EXISTS (
                 SELECT 1 FROM memory_tombstones t WHERE t.target_id = r.id
             ) AND NOT EXISTS (
                 SELECT 1 FROM memory_records n WHERE n.supersedes = r.id
             )",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(provider_error)?;
    let sessions = connection
        .query_row(
            "SELECT count(DISTINCT session_id) FROM session_entries",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(provider_error)?;
    let vector_extension_version = connection
        .query_row("SELECT vec_version()", [], |row| row.get::<_, String>(0))
        .map_err(provider_error)?;
    let embedding_config = connection
        .query_row(
            "SELECT model, revision, dimensions, distance_metric
             FROM memory_embedding_index WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()
        .map_err(provider_error)?;
    let embedding = if let Some((model, revision, dimensions, distance_metric)) = embedding_config {
        let indexed_records = connection
            .query_row(
                "SELECT count(*) FROM memory_vec v
                 JOIN memory_records r ON r.id = v.record_id
                 WHERE NOT EXISTS (
                     SELECT 1 FROM memory_tombstones t WHERE t.target_id = r.id
                 ) AND NOT EXISTS (
                     SELECT 1 FROM memory_records n WHERE n.supersedes = r.id
                 )",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(provider_error)?;
        let indexed_records = usize::try_from(indexed_records).map_err(|_| {
            MemoryError::Provider(format!("invalid vector row count: {indexed_records}"))
        })?;
        let active_count = usize::try_from(active_records).map_err(|_| {
            MemoryError::Provider(format!("invalid active record count: {active_records}"))
        })?;
        Some(MemoryEmbeddingHealth {
            model,
            revision,
            dimensions: usize::try_from(dimensions).map_err(|_| {
                MemoryError::Provider(format!("invalid embedding dimensions: {dimensions}"))
            })?,
            distance_metric,
            indexed_records,
            pending_records: active_count.saturating_sub(indexed_records),
        })
    } else {
        None
    };
    Ok(MemoryHealth {
        database_bytes: std::fs::metadata(&database_path).map_or(0, |metadata| metadata.len()),
        database_path,
        schema_version,
        integrity_check,
        mutations: count("memory_mutations")?,
        records: count("memory_records")?,
        active_records: usize::try_from(active_records).map_err(|_| {
            MemoryError::Provider(format!("invalid active record count: {active_records}"))
        })?,
        tombstones: count("memory_tombstones")?,
        sessions: usize::try_from(sessions)
            .map_err(|_| MemoryError::Provider(format!("invalid session count: {sessions}")))?,
        session_entries: count("session_entries")?,
        vector_extension_version,
        embedding,
        recovered_from,
    })
}

fn recall(
    connection: &Connection,
    query: RecallQuery,
    ranking: SqliteRecallRanking,
    query_embeddings: Option<&[Vec<u8>]>,
) -> Result<RecallResult, MemoryError> {
    recall_internal(connection, query, ranking, query_embeddings, None)
}

fn recall_with_candidates(
    connection: &Connection,
    query: RecallQuery,
    ranking: SqliteRecallRanking,
    query_embeddings: Option<&[Vec<u8>]>,
) -> Result<SqliteRecallCandidates, MemoryError> {
    let mut candidates = RecallCandidateIds::default();
    let result = recall_internal(
        connection,
        query,
        ranking,
        query_embeddings,
        Some(&mut candidates),
    )?;
    Ok(SqliteRecallCandidates {
        result,
        sparse_record_ids: candidates.sparse_record_ids,
        dense_record_ids: candidates.dense_record_ids,
        ranking_stages: candidates.ranking_stages,
    })
}

#[derive(Default)]
struct RecallCandidateIds {
    sparse_record_ids: Vec<String>,
    dense_record_ids: Vec<String>,
    ranking_stages: Option<SqliteRankingStages>,
}

fn recall_internal(
    connection: &Connection,
    query: RecallQuery,
    ranking: SqliteRecallRanking,
    query_embeddings: Option<&[Vec<u8>]>,
    mut candidates: Option<&mut RecallCandidateIds>,
) -> Result<RecallResult, MemoryError> {
    if query.scopes.is_empty() || query.limit == 0 {
        return Ok(RecallResult::default());
    }
    let scope_keys = query
        .scopes
        .iter()
        .map(MemoryScope::key)
        .collect::<Vec<_>>();
    let limit = query.limit.clamp(1, 100);
    let query_text = query.text.trim();
    let mut hits = if query_text.is_empty() {
        recall_recent(connection, &scope_keys, limit)?
    } else {
        let candidate_limit = match ranking {
            SqliteRecallRanking::Bm25 => limit,
            SqliteRecallRanking::Hybrid | SqliteRecallRanking::SparseDenseRawRrf => {
                crate::ranking::candidate_limit(limit)
            }
            SqliteRecallRanking::SparseDenseRrf => {
                crate::ranking::product_candidate_limit(query_text, limit)
            }
        };
        let sparse = build_fts_query(query_text).map_or_else(
            || Ok(Vec::new()),
            |fts_query| recall_fts(connection, &scope_keys, &fts_query, candidate_limit),
        )?;
        if let Some(candidates) = candidates.as_deref_mut() {
            candidates.sparse_record_ids = sparse.iter().map(|hit| hit.record.id.clone()).collect();
        }
        match ranking {
            SqliteRecallRanking::Bm25 => sparse,
            SqliteRecallRanking::Hybrid => crate::ranking::rerank(query_text, sparse, limit),
            SqliteRecallRanking::SparseDenseRawRrf => {
                let dense = query_embeddings
                    .and_then(|embeddings| embeddings.first())
                    .map_or_else(
                        || Ok(Vec::new()),
                        |embedding| {
                            recall_dense(connection, &scope_keys, embedding, candidate_limit)
                        },
                    )?;
                if let Some(candidates) = candidates.as_deref_mut() {
                    candidates.dense_record_ids =
                        dense.iter().map(|hit| hit.record.id.clone()).collect();
                }
                crate::ranking::fuse_raw_rrf(sparse, dense, limit)
            }
            SqliteRecallRanking::SparseDenseRrf => {
                let dense_candidate_limit = crate::ranking::product_dense_candidate_limit(
                    query_text,
                    candidate_limit,
                    sparse.len(),
                );
                let dense = query_embeddings.map_or_else(
                    || Ok(DenseVariantRecall::default()),
                    |embeddings| {
                        recall_dense_variants(
                            connection,
                            &scope_keys,
                            embeddings,
                            dense_candidate_limit,
                            limit.saturating_mul(2),
                        )
                    },
                )?;
                if let Some(candidates) = candidates.as_deref_mut() {
                    candidates.dense_record_ids =
                        dense.hits.iter().map(|hit| hit.record.id.clone()).collect();
                }
                if candidates.is_some() {
                    let (hits, stages) = crate::ranking::fuse_sparse_dense_with_stages(
                        query_text,
                        sparse,
                        dense.hits,
                        &dense.facet_matches,
                        limit,
                    );
                    candidates
                        .as_deref_mut()
                        .expect("candidate diagnostics requested")
                        .ranking_stages = Some(SqliteRankingStages {
                        protected_core_record_ids: stages.protected_core_record_ids,
                        gate_eligible_record_ids: stages.gate_eligible_record_ids,
                        pre_cutoff_record_ids: stages.pre_cutoff_record_ids,
                    });
                    hits
                } else {
                    crate::ranking::fuse_sparse_dense_with_facets(
                        query_text,
                        sparse,
                        dense.hits,
                        &dense.facet_matches,
                        limit,
                    )
                }
            }
        }
    };
    if hits.is_empty() && !query_text.is_empty() {
        hits = recall_like(connection, &scope_keys, query_text, limit)?;
        if let Some(candidates) = candidates {
            for hit in &hits {
                if !candidates.sparse_record_ids.contains(&hit.record.id) {
                    candidates.sparse_record_ids.push(hit.record.id.clone());
                }
            }
        }
    }
    Ok(RecallResult { hits })
}

fn recall_dense(
    connection: &Connection,
    scope_keys: &[String],
    query_embedding: &[u8],
    limit: usize,
) -> Result<Vec<MemoryHit>, MemoryError> {
    if !table_exists(connection, "memory_vec").map_err(provider_error)? {
        return Ok(Vec::new());
    }
    let placeholders = std::iter::repeat_n("?", scope_keys.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT record_id, distance FROM memory_vec
         WHERE embedding MATCH ? AND k = ? AND scope_key IN ({placeholders})
         ORDER BY distance"
    );
    let mut values = vec![
        rusqlite::types::Value::Blob(query_embedding.to_vec()),
        rusqlite::types::Value::Integer(limit as i64),
    ];
    values.extend(scope_keys.iter().cloned().map(rusqlite::types::Value::Text));
    let mut statement = connection.prepare(&sql).map_err(provider_error)?;
    let rows = statement
        .query_map(params_from_iter(values), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
        })
        .map_err(provider_error)?;
    let mut ranked_ids = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(provider_error)?;
    ranked_ids.sort_by(|left, right| {
        left.1
            .total_cmp(&right.1)
            .then_with(|| left.0.cmp(&right.0))
    });

    let active_sql = format!(
        "SELECT r.payload_json FROM memory_records r
         WHERE r.id = ? AND {}",
        active_memory_predicate(scope_keys.len())
    );
    let mut active_statement = connection
        .prepare_cached(&active_sql)
        .map_err(provider_error)?;
    let mut hits = Vec::with_capacity(ranked_ids.len());
    for (record_id, distance) in ranked_ids {
        let mut record_values = vec![rusqlite::types::Value::Text(record_id)];
        record_values.extend(scope_keys.iter().cloned().map(rusqlite::types::Value::Text));
        let payload = active_statement
            .query_row(params_from_iter(record_values), |row| {
                row.get::<_, String>(0)
            })
            .optional()
            .map_err(provider_error)?;
        if let Some(payload) = payload {
            hits.push(MemoryHit {
                record: serde_json::from_str(&payload).map_err(provider_error)?,
                score: -distance,
            });
        }
    }
    Ok(hits)
}

#[derive(Default)]
struct DenseVariantRecall {
    hits: Vec<MemoryHit>,
    facet_matches: BTreeMap<String, crate::ranking::SemanticFacetMatch>,
}

fn recall_dense_variants(
    connection: &Connection,
    scope_keys: &[String],
    query_embeddings: &[Vec<u8>],
    candidate_limit: usize,
    facet_limit: usize,
) -> Result<DenseVariantRecall, MemoryError> {
    struct FusedDenseCandidate {
        hit: MemoryHit,
        reciprocal_rank_score: f64,
        best_rank: usize,
        has_full_query_score: bool,
    }

    let Some((full_embedding, facet_embeddings)) = query_embeddings.split_first() else {
        return Ok(DenseVariantRecall::default());
    };
    let mut fused_by_record = BTreeMap::<String, FusedDenseCandidate>::new();
    let mut facet_matches = BTreeMap::<String, crate::ranking::SemanticFacetMatch>::new();
    let full_hits = recall_dense(connection, scope_keys, full_embedding, candidate_limit)?;
    let full_scores = full_hits
        .iter()
        .map(|hit| (hit.record.id.clone(), hit.score))
        .collect::<HashMap<_, _>>();
    for (rank, hit) in full_hits.into_iter().enumerate() {
        fused_by_record.insert(
            hit.record.id.clone(),
            FusedDenseCandidate {
                hit,
                reciprocal_rank_score: 1.0 / (DENSE_VARIANT_RRF_K + (rank + 1) as f64),
                best_rank: rank,
                has_full_query_score: true,
            },
        );
    }
    for (facet_index, embedding) in facet_embeddings.iter().enumerate() {
        for (rank, hit) in recall_dense(connection, scope_keys, embedding, candidate_limit)?
            .into_iter()
            .enumerate()
        {
            if rank < facet_limit
                && full_scores
                    .get(&hit.record.id)
                    .is_none_or(|full_score| hit.score > *full_score)
            {
                let facet_bit = 1_u64 << facet_index;
                facet_matches
                    .entry(hit.record.id.clone())
                    .and_modify(|facet_match| {
                        if rank < facet_match.best_rank {
                            facet_match.best_rank = rank;
                            facet_match.primary_mask = facet_bit;
                        }
                    })
                    .or_insert(crate::ranking::SemanticFacetMatch {
                        best_rank: rank,
                        primary_mask: facet_bit,
                    });
            }
            let contribution = 1.0 / (DENSE_VARIANT_RRF_K + (rank + 1) as f64);
            let candidate = fused_by_record
                .entry(hit.record.id.clone())
                .or_insert_with(|| FusedDenseCandidate {
                    hit: hit.clone(),
                    reciprocal_rank_score: 0.0,
                    best_rank: rank,
                    has_full_query_score: false,
                });
            candidate.reciprocal_rank_score += contribution;
            candidate.best_rank = candidate.best_rank.min(rank);
            if !candidate.has_full_query_score && hit.score > candidate.hit.score {
                candidate.hit = hit;
            }
        }
    }
    let mut candidates = fused_by_record.into_values().collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .reciprocal_rank_score
            .total_cmp(&left.reciprocal_rank_score)
            .then_with(|| left.best_rank.cmp(&right.best_rank))
            .then_with(|| right.hit.score.total_cmp(&left.hit.score))
            .then_with(|| left.hit.record.id.cmp(&right.hit.record.id))
    });
    candidates.truncate(candidate_limit);
    let hits = candidates
        .into_iter()
        .map(|candidate| candidate.hit)
        .collect::<Vec<_>>();
    let retained = hits
        .iter()
        .map(|hit| hit.record.id.as_str())
        .collect::<BTreeSet<_>>();
    facet_matches.retain(|record_id, _| retained.contains(record_id.as_str()));
    Ok(DenseVariantRecall {
        hits,
        facet_matches,
    })
}

fn active_memory_predicate(scope_count: usize) -> String {
    let placeholders = std::iter::repeat_n("?", scope_count)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "r.scope_key IN ({placeholders}) \
         AND NOT EXISTS (SELECT 1 FROM memory_tombstones t WHERE t.target_id = r.id) \
         AND NOT EXISTS (SELECT 1 FROM memory_records n WHERE n.supersedes = r.id)"
    )
}

fn recall_recent(
    connection: &Connection,
    scope_keys: &[String],
    limit: usize,
) -> Result<Vec<MemoryHit>, MemoryError> {
    let sql = format!(
        "SELECT r.payload_json FROM memory_records r WHERE {} \
         ORDER BY r.recorded_at_ms DESC LIMIT ?",
        active_memory_predicate(scope_keys.len())
    );
    let mut values = scope_keys
        .iter()
        .cloned()
        .map(rusqlite::types::Value::Text)
        .collect::<Vec<_>>();
    values.push(rusqlite::types::Value::Integer(limit as i64));
    query_memory_hits(connection, &sql, values, false)
}

fn recall_fts(
    connection: &Connection,
    scope_keys: &[String],
    fts_query: &str,
    limit: usize,
) -> Result<Vec<MemoryHit>, MemoryError> {
    let sql = format!(
        "SELECT r.payload_json, bm25(memory_fts) \
         FROM memory_fts JOIN memory_records r ON r.id = memory_fts.record_id \
         WHERE memory_fts MATCH ? AND {} \
         ORDER BY bm25(memory_fts), r.recorded_at_ms DESC LIMIT ?",
        active_memory_predicate(scope_keys.len())
    );
    let mut values = vec![rusqlite::types::Value::Text(fts_query.to_string())];
    values.extend(scope_keys.iter().cloned().map(rusqlite::types::Value::Text));
    values.push(rusqlite::types::Value::Integer(limit as i64));
    query_memory_hits(connection, &sql, values, true)
}

fn recall_like(
    connection: &Connection,
    scope_keys: &[String],
    text: &str,
    limit: usize,
) -> Result<Vec<MemoryHit>, MemoryError> {
    let sql = format!(
        "SELECT r.payload_json FROM memory_records r WHERE r.text LIKE ? ESCAPE '\\' AND {} \
         ORDER BY r.recorded_at_ms DESC LIMIT ?",
        active_memory_predicate(scope_keys.len())
    );
    let mut values = vec![rusqlite::types::Value::Text(format!(
        "%{}%",
        escape_like(text)
    ))];
    values.extend(scope_keys.iter().cloned().map(rusqlite::types::Value::Text));
    values.push(rusqlite::types::Value::Integer(limit as i64));
    query_memory_hits(connection, &sql, values, false)
}

fn query_memory_hits(
    connection: &Connection,
    sql: &str,
    values: Vec<rusqlite::types::Value>,
    has_rank: bool,
) -> Result<Vec<MemoryHit>, MemoryError> {
    let mut statement = connection.prepare(sql).map_err(provider_error)?;
    let rows = statement
        .query_map(params_from_iter(values), |row| {
            let payload: String = row.get(0)?;
            let rank = if has_rank { row.get::<_, f64>(1)? } else { 0.0 };
            Ok((payload, rank))
        })
        .map_err(provider_error)?;
    rows.map(|row| {
        let (payload, rank) = row.map_err(provider_error)?;
        let record = serde_json::from_str(&payload).map_err(provider_error)?;
        Ok(MemoryHit {
            record,
            score: if has_rank { -rank } else { 0.0 },
        })
    })
    .collect()
}

fn replace_session(
    connection: &mut Connection,
    document: SessionIndexDocument,
) -> Result<(), MemoryError> {
    let transaction = connection.transaction().map_err(provider_error)?;
    replace_session_in_transaction(&transaction, document)?;
    transaction.commit().map_err(provider_error)
}

fn replace_session_in_transaction(
    transaction: &Transaction<'_>,
    document: SessionIndexDocument,
) -> Result<(), MemoryError> {
    let document_ids = {
        let mut statement = transaction
            .prepare("SELECT document_id FROM session_entries WHERE session_id = ?1")
            .map_err(provider_error)?;
        let rows = statement
            .query_map(params![document.session_id], |row| row.get::<_, String>(0))
            .map_err(provider_error)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(provider_error)?
    };
    for document_id in document_ids {
        transaction
            .execute(
                "DELETE FROM session_fts WHERE document_id = ?1",
                params![document_id],
            )
            .map_err(provider_error)?;
    }
    transaction
        .execute(
            "DELETE FROM session_entries WHERE session_id = ?1",
            params![document.session_id],
        )
        .map_err(provider_error)?;
    for entry in document.entries {
        if entry.text.trim().is_empty() {
            continue;
        }
        let document_id = format!("{}:{}", document.session_id, entry.entry_id);
        transaction
            .execute(
                "INSERT INTO session_entries(\
                     document_id, session_id, project_key, entry_id, role, text, timestamp_ms\
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    document_id,
                    document.session_id,
                    document.project_key,
                    entry.entry_id,
                    entry.role,
                    entry.text,
                    entry.timestamp_ms,
                ],
            )
            .map_err(provider_error)?;
        transaction
            .execute(
                "INSERT INTO session_fts(document_id, text) VALUES (?1, ?2)",
                params![document_id, entry.text],
            )
            .map_err(provider_error)?;
    }
    Ok(())
}

fn search_sessions(
    connection: &Connection,
    query: SessionSearchQuery,
) -> Result<Vec<SessionSearchHit>, MemoryError> {
    if query.text.trim().is_empty() || query.limit == 0 {
        return Ok(Vec::new());
    }
    let limit = query.limit.clamp(1, 100);
    let hits = build_fts_query(&query.text).map_or_else(
        || Ok(Vec::new()),
        |fts_query| search_sessions_fts(connection, &query, &fts_query, limit),
    )?;
    if hits.is_empty() {
        search_sessions_like(connection, &query, limit)
    } else {
        Ok(hits)
    }
}

fn search_sessions_fts(
    connection: &Connection,
    query: &SessionSearchQuery,
    fts_query: &str,
    limit: usize,
) -> Result<Vec<SessionSearchHit>, MemoryError> {
    let session_filter = query.session_id.as_ref().map(|_| " AND s.session_id = ?");
    let sql = format!(
        "SELECT s.session_id, s.entry_id, s.role, s.text, s.timestamp_ms, bm25(session_fts) \
         FROM session_fts JOIN session_entries s ON s.document_id = session_fts.document_id \
         WHERE session_fts MATCH ? AND s.project_key = ?{} \
         ORDER BY bm25(session_fts), s.timestamp_ms DESC LIMIT ?",
        session_filter.unwrap_or_default()
    );
    let mut values = vec![
        rusqlite::types::Value::Text(fts_query.to_string()),
        rusqlite::types::Value::Text(query.project_key.clone()),
    ];
    if let Some(session_id) = &query.session_id {
        values.push(rusqlite::types::Value::Text(session_id.clone()));
    }
    values.push(rusqlite::types::Value::Integer(limit as i64));
    query_session_hits(connection, &sql, values, true)
}

fn search_sessions_like(
    connection: &Connection,
    query: &SessionSearchQuery,
    limit: usize,
) -> Result<Vec<SessionSearchHit>, MemoryError> {
    let session_filter = query.session_id.as_ref().map(|_| " AND session_id = ?");
    let sql = format!(
        "SELECT session_id, entry_id, role, text, timestamp_ms \
         FROM session_entries WHERE text LIKE ? ESCAPE '\\' AND project_key = ?{} \
         ORDER BY timestamp_ms DESC LIMIT ?",
        session_filter.unwrap_or_default()
    );
    let mut values = vec![
        rusqlite::types::Value::Text(format!("%{}%", escape_like(query.text.trim()))),
        rusqlite::types::Value::Text(query.project_key.clone()),
    ];
    if let Some(session_id) = &query.session_id {
        values.push(rusqlite::types::Value::Text(session_id.clone()));
    }
    values.push(rusqlite::types::Value::Integer(limit as i64));
    query_session_hits(connection, &sql, values, false)
}

fn query_session_hits(
    connection: &Connection,
    sql: &str,
    values: Vec<rusqlite::types::Value>,
    has_rank: bool,
) -> Result<Vec<SessionSearchHit>, MemoryError> {
    let mut statement = connection.prepare(sql).map_err(provider_error)?;
    let rows = statement
        .query_map(params_from_iter(values), |row| {
            Ok(SessionSearchHit {
                session_id: row.get(0)?,
                entry_id: row.get(1)?,
                role: row.get(2)?,
                text: row.get(3)?,
                timestamp_ms: row.get(4)?,
                score: if has_rank {
                    -row.get::<_, f64>(5)?
                } else {
                    0.0
                },
            })
        })
        .map_err(provider_error)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(provider_error)
}

fn build_fts_query(text: &str) -> Option<String> {
    let terms = text
        .split(|character: char| character.is_whitespace() || character.is_ascii_punctuation())
        .map(str::trim)
        .filter(|term| !term.is_empty())
        .take(16)
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>();
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

fn escape_like(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn provider_error(error: impl std::fmt::Display) -> MemoryError {
    MemoryError::Provider(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::{
        EmbeddingError, MemoryEvidence, MemoryKind, MemoryOrigin, MemoryScope, SessionIndexEntry,
    };

    #[derive(Debug)]
    struct TestEmbedder {
        descriptor: EmbeddingDescriptor,
        fail: Arc<AtomicBool>,
    }

    impl TestEmbedder {
        fn new(fail: bool) -> Self {
            Self {
                descriptor: EmbeddingDescriptor {
                    model: "deterministic-test".to_string(),
                    revision: "v1".to_string(),
                    dimensions: 2,
                },
                fail: Arc::new(AtomicBool::new(fail)),
            }
        }
    }

    #[async_trait::async_trait]
    impl MemoryEmbedder for TestEmbedder {
        fn descriptor(&self) -> &EmbeddingDescriptor {
            &self.descriptor
        }

        async fn embed(
            &self,
            purpose: EmbeddingPurpose,
            texts: Vec<String>,
        ) -> Result<Vec<Vec<f32>>, EmbeddingError> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(EmbeddingError::Provider("injected failure".to_string()));
            }
            Ok(texts
                .into_iter()
                .map(|text| match purpose {
                    EmbeddingPurpose::Query => vec![1.0, 0.0],
                    EmbeddingPurpose::Document if text.contains("wrong-scope") => vec![1.0, 0.0],
                    EmbeddingPurpose::Document if text.contains("ultramarine") => {
                        vec![0.9, 0.1]
                    }
                    EmbeddingPurpose::Document => vec![0.0, 1.0],
                })
                .collect())
        }
    }

    fn record(id: &str, text: &str, supersedes: Option<&str>) -> MemoryRecord {
        MemoryRecord {
            id: id.to_string(),
            scope: MemoryScope::Project {
                root: "/repo".to_string(),
            },
            kind: MemoryKind::Fact,
            text: text.to_string(),
            origin: MemoryOrigin {
                session_id: "session".to_string(),
                entry_id: Some("entry".to_string()),
                tool_call_id: None,
            },
            evidence: MemoryEvidence {
                note: "test evidence".to_string(),
            },
            recorded_at_ms: 1,
            supersedes: supersedes.map(str::to_string),
        }
    }

    fn scoped_record(id: &str, text: &str, root: &str) -> MemoryRecord {
        let mut record = record(id, text, None);
        record.scope = MemoryScope::Project {
            root: root.to_string(),
        };
        record
    }

    fn remember(mutation_id: &str, record: MemoryRecord) -> MemoryMutation {
        MemoryMutation::Remember {
            mutation_id: mutation_id.to_string(),
            record,
        }
    }

    #[tokio::test]
    async fn applies_idempotently_and_hides_forgotten_records() {
        let directory = tempfile::tempdir().unwrap();
        let provider = LocalMemoryProvider::open(directory.path().join("memory.sqlite3")).unwrap();
        let mutation = remember("mutation-1", record("record-1", "Prefer Rust", None));
        let receipt = provider.apply(vec![mutation.clone()]).await.unwrap();
        assert_eq!(receipt.applied, 1);
        let duplicate = provider.apply(vec![mutation]).await.unwrap();
        assert_eq!(duplicate.duplicates, 1);

        let recalled = provider
            .recall(RecallQuery {
                text: "Rust".to_string(),
                scopes: vec![MemoryScope::Project {
                    root: "/repo".to_string(),
                }],
                limit: 10,
            })
            .await
            .unwrap();
        assert_eq!(recalled.hits[0].record.id, "record-1");

        provider
            .apply(vec![MemoryMutation::Forget {
                mutation_id: "mutation-2".to_string(),
                target_id: "record-1".to_string(),
                reason: "obsolete".to_string(),
                origin: MemoryOrigin {
                    session_id: "session".to_string(),
                    entry_id: None,
                    tool_call_id: None,
                },
                recorded_at_ms: 2,
            }])
            .await
            .unwrap();
        assert!(
            provider
                .recall(RecallQuery {
                    text: String::new(),
                    scopes: vec![MemoryScope::Project {
                        root: "/repo".to_string(),
                    }],
                    limit: 10,
                })
                .await
                .unwrap()
                .hits
                .is_empty()
        );
    }

    #[tokio::test]
    async fn correction_supersedes_the_old_record_regardless_of_replay_order() {
        let directory = tempfile::tempdir().unwrap();
        let provider = LocalMemoryProvider::open(directory.path().join("memory.sqlite3")).unwrap();
        provider
            .apply(vec![
                remember(
                    "mutation-new",
                    record("record-new", "Use stable Rust", Some("record-old")),
                ),
                remember(
                    "mutation-old",
                    record("record-old", "Use nightly Rust", None),
                ),
            ])
            .await
            .unwrap();
        let recalled = provider
            .recall(RecallQuery {
                text: "Rust".to_string(),
                scopes: vec![MemoryScope::Project {
                    root: "/repo".to_string(),
                }],
                limit: 10,
            })
            .await
            .unwrap();
        assert_eq!(
            recalled
                .hits
                .iter()
                .map(|hit| hit.record.id.as_str())
                .collect::<Vec<_>>(),
            vec!["record-new"]
        );
    }

    #[tokio::test]
    async fn dense_knn_prefilters_scope_before_the_candidate_limit() {
        let directory = tempfile::tempdir().unwrap();
        let provider = LocalMemoryProvider::open_with_embedder(
            directory.path().join("memory.sqlite3"),
            Arc::new(TestEmbedder::new(false)),
        )
        .unwrap();
        let mut mutations = (0..80)
            .map(|index| {
                remember(
                    &format!("wrong-mutation-{index}"),
                    scoped_record(
                        &format!("wrong-record-{index}"),
                        &format!("wrong-scope distractor {index}"),
                        "/other",
                    ),
                )
            })
            .collect::<Vec<_>>();
        mutations.push(remember(
            "target-mutation",
            scoped_record(
                "target-record",
                "The deployment hue is ultramarine.",
                "/repo",
            ),
        ));
        provider.apply(mutations).await.unwrap();

        let recalled = provider
            .recall(RecallQuery {
                text: "semantic lookup with no shared terms".to_string(),
                scopes: vec![MemoryScope::Project {
                    root: "/repo".to_string(),
                }],
                limit: 5,
            })
            .await
            .unwrap();

        assert_eq!(
            recalled
                .hits
                .iter()
                .map(|hit| hit.record.id.as_str())
                .collect::<Vec<_>>(),
            ["target-record"]
        );
    }

    #[tokio::test]
    async fn dense_recall_does_not_dilute_a_high_confidence_lexical_result() {
        let directory = tempfile::tempdir().unwrap();
        let provider = LocalMemoryProvider::open_with_embedder(
            directory.path().join("memory.sqlite3"),
            Arc::new(TestEmbedder::new(false)),
        )
        .unwrap();
        provider
            .apply(vec![
                remember(
                    "target-mutation",
                    record(
                        "target-record",
                        "Atlas full test command is cargo test --workspace.",
                        None,
                    ),
                ),
                remember(
                    "dense-distractor-mutation",
                    record(
                        "dense-distractor",
                        "The deployment hue is ultramarine.",
                        None,
                    ),
                ),
            ])
            .await
            .unwrap();

        let recalled = provider
            .recall(RecallQuery {
                text: "Atlas full test command cargo workspace".to_string(),
                scopes: vec![MemoryScope::Project {
                    root: "/repo".to_string(),
                }],
                limit: 5,
            })
            .await
            .unwrap();

        assert_eq!(
            recalled
                .hits
                .iter()
                .map(|hit| hit.record.id.as_str())
                .collect::<Vec<_>>(),
            ["target-record"]
        );
    }

    #[tokio::test]
    async fn concrete_adapter_reports_candidates_without_changing_product_results() {
        let directory = tempfile::tempdir().unwrap();
        let provider = LocalMemoryProvider::open_with_embedder(
            directory.path().join("memory.sqlite3"),
            Arc::new(TestEmbedder::new(false)),
        )
        .unwrap();
        provider
            .apply(vec![
                remember(
                    "target-mutation",
                    record(
                        "target-record",
                        "Atlas full test command is cargo test --workspace.",
                        None,
                    ),
                ),
                remember(
                    "dense-distractor-mutation",
                    record(
                        "dense-distractor",
                        "The deployment hue is ultramarine.",
                        None,
                    ),
                ),
            ])
            .await
            .unwrap();

        let traced = provider
            .recall_with_candidates(RecallQuery {
                text: "Atlas full test command cargo workspace".to_string(),
                scopes: vec![MemoryScope::Project {
                    root: "/repo".to_string(),
                }],
                limit: 5,
            })
            .await
            .unwrap();

        assert_eq!(
            traced
                .result
                .hits
                .iter()
                .map(|hit| hit.record.id.as_str())
                .collect::<Vec<_>>(),
            ["target-record"]
        );
        assert_eq!(traced.sparse_record_ids, ["target-record"]);
        assert_eq!(
            traced.dense_record_ids,
            ["dense-distractor", "target-record"]
        );
        let stages = traced.ranking_stages.expect("hybrid ranking stages");
        assert_eq!(stages.protected_core_record_ids, ["target-record"]);
        assert_eq!(stages.gate_eligible_record_ids, ["target-record"]);
        assert_eq!(stages.pre_cutoff_record_ids, ["target-record"]);
    }

    #[tokio::test]
    async fn raw_rrf_control_exposes_the_dilution_prevented_by_product_ranking() {
        let directory = tempfile::tempdir().unwrap();
        let provider = LocalMemoryProvider::open_with_embedder_and_ranking(
            directory.path().join("memory.sqlite3"),
            Arc::new(TestEmbedder::new(false)),
            SqliteRecallRanking::SparseDenseRawRrf,
        )
        .unwrap();
        provider
            .apply(vec![
                remember(
                    "target-mutation",
                    record(
                        "target-record",
                        "Atlas full test command is cargo test --workspace.",
                        None,
                    ),
                ),
                remember(
                    "dense-distractor-mutation",
                    record(
                        "dense-distractor",
                        "The deployment hue is ultramarine.",
                        None,
                    ),
                ),
            ])
            .await
            .unwrap();

        let recalled = provider
            .recall(RecallQuery {
                text: "Atlas full test command cargo workspace".to_string(),
                scopes: vec![MemoryScope::Project {
                    root: "/repo".to_string(),
                }],
                limit: 5,
            })
            .await
            .unwrap();

        assert_eq!(
            recalled
                .hits
                .iter()
                .map(|hit| hit.record.id.as_str())
                .collect::<Vec<_>>(),
            ["target-record", "dense-distractor"]
        );
    }

    #[tokio::test]
    async fn dense_rescue_must_add_query_evidence_beyond_the_lexical_core() {
        let directory = tempfile::tempdir().unwrap();
        let embedder = Arc::new(TestEmbedder::new(false));
        let provider = LocalMemoryProvider::open_with_embedder(
            directory.path().join("memory.sqlite3"),
            embedder.clone(),
        )
        .unwrap();
        provider
            .apply(vec![
                remember(
                    "target-mutation",
                    record(
                        "target-record",
                        "Atlas registered tools are read, grep, and bash; there is no deploy tool. The inventory includes alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima.",
                        None,
                    ),
                ),
                remember(
                    "dense-distractor-mutation",
                    record(
                        "dense-distractor",
                        "Atlas ultramarine note routes to the staging area before teams deploy.",
                        None,
                    ),
                ),
            ])
            .await
            .unwrap();

        let query = RecallQuery {
            text: "How do I invoke the Atlas deploy tool, and which tools are actually registered to me?"
                .to_owned()
                + " alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima",
            scopes: vec![MemoryScope::Project {
                root: "/repo".to_string(),
            }],
            limit: 5,
        };
        embedder.fail.store(true, Ordering::SeqCst);
        let lexical = provider.recall(query.clone()).await.unwrap();
        assert_eq!(
            lexical
                .hits
                .iter()
                .map(|hit| hit.record.id.as_str())
                .collect::<Vec<_>>(),
            ["target-record"]
        );

        embedder.fail.store(false, Ordering::SeqCst);
        let recalled = provider.recall(query).await.unwrap();

        assert_eq!(
            recalled
                .hits
                .iter()
                .map(|hit| hit.record.id.as_str())
                .collect::<Vec<_>>(),
            ["target-record"]
        );
    }

    #[tokio::test]
    async fn dense_rescue_can_add_complementary_query_evidence() {
        let directory = tempfile::tempdir().unwrap();
        let embedder = Arc::new(TestEmbedder::new(false));
        let provider = LocalMemoryProvider::open_with_embedder(
            directory.path().join("memory.sqlite3"),
            embedder.clone(),
        )
        .unwrap();
        provider
            .apply(vec![
                remember(
                    "core-mutation",
                    record(
                        "core-record",
                        "Atlas release workflow builds a signed artifact, stages production, records approval, and verifies canonical validation policy checksum provenance owner region.",
                        None,
                    ),
                ),
                remember(
                    "rescue-mutation",
                    record(
                        "rescue-record",
                        "The workspace hue is ultramarine.",
                        None,
                    ),
                ),
            ])
            .await
            .unwrap();
        let query = RecallQuery {
            text: "Atlas release workflow signed artifact stages production approval canonical validation policy checksum provenance owner region workspace ultramarine"
                .to_string(),
            scopes: vec![MemoryScope::Project {
                root: "/repo".to_string(),
            }],
            limit: 5,
        };

        embedder.fail.store(true, Ordering::SeqCst);
        let lexical = provider.recall(query.clone()).await.unwrap();
        assert_eq!(
            lexical
                .hits
                .iter()
                .map(|hit| hit.record.id.as_str())
                .collect::<Vec<_>>(),
            ["core-record"]
        );

        embedder.fail.store(false, Ordering::SeqCst);
        let hybrid = provider.recall(query).await.unwrap();
        assert_eq!(
            hybrid
                .hits
                .iter()
                .map(|hit| hit.record.id.as_str())
                .collect::<Vec<_>>(),
            ["core-record", "rescue-record"]
        );
    }

    #[tokio::test]
    async fn embedding_failure_keeps_sparse_memory_and_backfill_repairs_dense_index() {
        let directory = tempfile::tempdir().unwrap();
        let embedder = Arc::new(TestEmbedder::new(true));
        let provider = LocalMemoryProvider::open_with_embedder(
            directory.path().join("memory.sqlite3"),
            embedder.clone(),
        )
        .unwrap();
        provider
            .apply(vec![remember(
                "mutation",
                record(
                    "record",
                    "The deployment hue is ultramarine and Rust remains preferred.",
                    None,
                ),
            )])
            .await
            .unwrap();

        let sparse = provider
            .recall(RecallQuery {
                text: "Rust".to_string(),
                scopes: vec![MemoryScope::Project {
                    root: "/repo".to_string(),
                }],
                limit: 5,
            })
            .await
            .unwrap();
        assert_eq!(sparse.hits[0].record.id, "record");
        let health = provider.health().await.unwrap();
        let embedding = health.embedding.expect("embedding status");
        assert_eq!(embedding.indexed_records, 0);
        assert_eq!(embedding.pending_records, 1);

        embedder.fail.store(false, Ordering::SeqCst);
        let receipt = provider.backfill_embeddings(10).await.unwrap();
        assert_eq!(receipt.attempted, 1);
        assert_eq!(receipt.indexed, 1);
        assert_eq!(receipt.remaining, 0);
        let dense = provider
            .recall(RecallQuery {
                text: "semantic lookup with no shared terms".to_string(),
                scopes: vec![MemoryScope::Project {
                    root: "/repo".to_string(),
                }],
                limit: 5,
            })
            .await
            .unwrap();
        assert_eq!(dense.hits[0].record.id, "record");
    }

    #[tokio::test]
    async fn dense_index_excludes_superseded_records_in_reverse_replay_order() {
        let directory = tempfile::tempdir().unwrap();
        let provider = LocalMemoryProvider::open_with_embedder(
            directory.path().join("memory.sqlite3"),
            Arc::new(TestEmbedder::new(false)),
        )
        .unwrap();
        provider
            .apply(vec![
                remember(
                    "new-mutation",
                    record(
                        "new-record",
                        "Use the replacement deployment hue.",
                        Some("old-record"),
                    ),
                ),
                remember(
                    "old-mutation",
                    record("old-record", "The deployment hue is ultramarine.", None),
                ),
            ])
            .await
            .unwrap();

        let recalled = provider
            .recall(RecallQuery {
                text: "semantic lookup with no shared terms".to_string(),
                scopes: vec![MemoryScope::Project {
                    root: "/repo".to_string(),
                }],
                limit: 5,
            })
            .await
            .unwrap();
        assert!(
            recalled
                .hits
                .iter()
                .all(|hit| hit.record.id != "old-record")
        );
        let embedding = provider
            .health()
            .await
            .unwrap()
            .embedding
            .expect("embedding status");
        assert_eq!(embedding.indexed_records, 1);
        assert_eq!(embedding.pending_records, 0);
    }

    #[tokio::test]
    async fn session_index_replacement_removes_abandoned_branch_entries() {
        let directory = tempfile::tempdir().unwrap();
        let provider = LocalMemoryProvider::open(directory.path().join("memory.sqlite3")).unwrap();
        provider
            .replace_session(SessionIndexDocument {
                session_id: "session".to_string(),
                project_key: "/repo".to_string(),
                entries: vec![SessionIndexEntry {
                    entry_id: "old".to_string(),
                    role: "user".to_string(),
                    text: "abandoned branch phrase".to_string(),
                    timestamp_ms: 1,
                }],
            })
            .await
            .unwrap();
        provider
            .replace_session(SessionIndexDocument {
                session_id: "session".to_string(),
                project_key: "/repo".to_string(),
                entries: vec![SessionIndexEntry {
                    entry_id: "new".to_string(),
                    role: "assistant".to_string(),
                    text: "active branch phrase".to_string(),
                    timestamp_ms: 2,
                }],
            })
            .await
            .unwrap();
        let old = provider
            .search_sessions(SessionSearchQuery {
                text: "abandoned".to_string(),
                project_key: "/repo".to_string(),
                session_id: None,
                limit: 10,
            })
            .await
            .unwrap();
        assert!(old.is_empty());
        let current = provider
            .search_sessions(SessionSearchQuery {
                text: "active".to_string(),
                project_key: "/repo".to_string(),
                session_id: None,
                limit: 10,
            })
            .await
            .unwrap();
        assert_eq!(current[0].entry_id, "new");
    }

    #[tokio::test]
    async fn rejects_a_mutation_id_reused_for_a_different_payload() {
        let directory = tempfile::tempdir().unwrap();
        let provider = LocalMemoryProvider::open(directory.path().join("memory.sqlite3")).unwrap();
        provider
            .apply(vec![remember(
                "mutation",
                record("record-1", "Prefer Rust", None),
            )])
            .await
            .unwrap();

        let error = provider
            .apply(vec![remember(
                "mutation",
                record("record-2", "Prefer Go", None),
            )])
            .await
            .unwrap_err();

        assert!(error.to_string().contains("conflicting journal payloads"));
        let recalled = provider
            .recall(RecallQuery {
                text: String::new(),
                scopes: vec![MemoryScope::Project {
                    root: "/repo".to_string(),
                }],
                limit: 10,
            })
            .await
            .unwrap();
        assert_eq!(recalled.hits[0].record.id, "record-1");
    }

    #[tokio::test]
    async fn rebuild_replaces_every_derived_table_and_reports_health() {
        let directory = tempfile::tempdir().unwrap();
        let provider = LocalMemoryProvider::open(directory.path().join("memory.sqlite3")).unwrap();
        provider
            .apply(vec![remember(
                "stray-mutation",
                record("stray-record", "stray memory", None),
            )])
            .await
            .unwrap();
        provider
            .replace_session(SessionIndexDocument {
                session_id: "stray-session".to_string(),
                project_key: "/repo".to_string(),
                entries: vec![SessionIndexEntry {
                    entry_id: "stray-entry".to_string(),
                    role: "user".to_string(),
                    text: "stray transcript".to_string(),
                    timestamp_ms: 1,
                }],
            })
            .await
            .unwrap();

        let receipt = provider
            .rebuild_with(|| {
                Ok(MemoryRebuildBatch {
                    mutations: vec![remember(
                        "canonical-mutation",
                        record("canonical-record", "canonical memory", None),
                    )],
                    sessions: vec![SessionIndexDocument {
                        session_id: "canonical-session".to_string(),
                        project_key: "/repo".to_string(),
                        entries: vec![SessionIndexEntry {
                            entry_id: "canonical-entry".to_string(),
                            role: "assistant".to_string(),
                            text: "canonical transcript".to_string(),
                            timestamp_ms: 2,
                        }],
                    }],
                    source_files: 2,
                    skipped_files: 1,
                })
            })
            .await
            .unwrap();

        assert_eq!(receipt.source_files, 2);
        assert_eq!(receipt.skipped_files, 1);
        assert_eq!(receipt.mutations, 1);
        assert_eq!(receipt.sessions, 1);
        assert_eq!(receipt.session_entries, 1);
        assert!(
            provider
                .recall(RecallQuery {
                    text: "stray".to_string(),
                    scopes: vec![MemoryScope::Project {
                        root: "/repo".to_string(),
                    }],
                    limit: 10,
                })
                .await
                .unwrap()
                .hits
                .is_empty()
        );
        assert_eq!(
            provider
                .recall(RecallQuery {
                    text: "canonical".to_string(),
                    scopes: vec![MemoryScope::Project {
                        root: "/repo".to_string(),
                    }],
                    limit: 10,
                })
                .await
                .unwrap()
                .hits[0]
                .record
                .id,
            "canonical-record"
        );
        let health = provider.health().await.unwrap();
        assert!(health.is_healthy());
        assert_eq!(health.mutations, 1);
        assert_eq!(health.records, 1);
        assert_eq!(health.active_records, 1);
        assert_eq!(health.sessions, 1);
        assert_eq!(health.session_entries, 1);
    }

    #[tokio::test]
    async fn failed_rebuild_rolls_back_to_the_previous_index() {
        let directory = tempfile::tempdir().unwrap();
        let provider = LocalMemoryProvider::open(directory.path().join("memory.sqlite3")).unwrap();
        provider
            .apply(vec![remember(
                "old-mutation",
                record("old-record", "old memory survives", None),
            )])
            .await
            .unwrap();

        let error = provider
            .rebuild_with(|| {
                Ok(MemoryRebuildBatch {
                    mutations: vec![
                        remember("new-mutation-1", record("duplicate-record", "first", None)),
                        remember("new-mutation-2", record("duplicate-record", "second", None)),
                    ],
                    ..MemoryRebuildBatch::default()
                })
            })
            .await
            .unwrap_err();

        assert!(error.to_string().contains("UNIQUE constraint failed"));
        let recalled = provider
            .recall(RecallQuery {
                text: "survives".to_string(),
                scopes: vec![MemoryScope::Project {
                    root: "/repo".to_string(),
                }],
                limit: 10,
            })
            .await
            .unwrap();
        assert_eq!(recalled.hits[0].record.id, "old-record");
    }

    #[tokio::test]
    async fn writes_waiting_behind_a_rebuild_are_not_cleared() {
        let directory = tempfile::tempdir().unwrap();
        let provider = LocalMemoryProvider::open(directory.path().join("memory.sqlite3")).unwrap();
        let rebuild_provider = provider.clone();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let rebuild = tokio::spawn(async move {
            rebuild_provider
                .rebuild_with(move || {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(MemoryRebuildBatch::default())
                })
                .await
        });
        tokio::task::spawn_blocking(move || started_rx.recv().unwrap())
            .await
            .unwrap();
        let write_provider = provider.clone();
        let mut write = tokio::spawn(async move {
            write_provider
                .apply(vec![remember(
                    "concurrent-mutation",
                    record("concurrent-record", "written after rebuild scan", None),
                )])
                .await
        });

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut write)
                .await
                .is_err()
        );
        release_tx.send(()).unwrap();
        rebuild.await.unwrap().unwrap();
        write.await.unwrap().unwrap();

        let recalled = provider
            .recall(RecallQuery {
                text: "written".to_string(),
                scopes: vec![MemoryScope::Project {
                    root: "/repo".to_string(),
                }],
                limit: 10,
            })
            .await
            .unwrap();
        assert_eq!(recalled.hits[0].record.id, "concurrent-record");
    }

    #[tokio::test]
    async fn corrupt_database_is_preserved_before_a_clean_replacement_is_created() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("memory.sqlite3");
        std::fs::write(&path, b"this is not sqlite").unwrap();

        let provider = LocalMemoryProvider::open(&path).unwrap();
        let health = provider.health().await.unwrap();

        assert!(health.is_healthy());
        let backup = health
            .recovered_from
            .expect("corrupt database should be preserved");
        assert_eq!(std::fs::read(backup).unwrap(), b"this is not sqlite");
        assert!(path.is_file());
        provider
            .rebuild_with(|| Ok(MemoryRebuildBatch::default()))
            .await
            .unwrap();
        assert!(provider.health().await.unwrap().recovered_from.is_none());
    }

    #[test]
    fn future_schema_is_rejected_without_downgrading_the_database() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("memory.sqlite3");
        let connection = Connection::open(&path).unwrap();
        let future_version = SCHEMA_VERSION + 1;
        connection
            .pragma_update(None, "user_version", future_version)
            .unwrap();
        drop(connection);

        let error = LocalMemoryProvider::open(&path).unwrap_err();

        assert!(error.to_string().contains(&format!(
            "unsupported memory database schema {future_version}"
        )));
        let connection = Connection::open(path).unwrap();
        let version = connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap();
        assert_eq!(version, future_version);
    }
}
