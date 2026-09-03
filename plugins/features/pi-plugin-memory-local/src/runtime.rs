use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pi_core::{ContentBlock, Message, SessionEntryKind, SessionSnapshot};
use pi_memory_loader::MemoryRecallOptions;
use pi_session::{SessionEntry, SessionFileFormat, SessionLog, SessionRecord};

use crate::embedding::initialization::LocalMemoryProviderInitializer;
use crate::{
    ApplyReceipt, FastEmbedInstallReceipt, FastEmbedModelStatus, LocalMemoryProvider,
    MEMORY_EVENT_TYPE, MemoryEmbeddingBackfillReceipt, MemoryError, MemoryHealth, MemoryMutation,
    MemoryRebuildBatch, MemoryRebuildReceipt, MemoryScope, RecallQuery, RecallResult,
    SessionIndexDocument, SessionIndexEntry, SessionSearchHit, SessionSearchQuery,
};

const MAX_INDEXED_ENTRY_BYTES: usize = 64 * 1024;
const EMBEDDING_BACKFILL_BATCH_SIZE: usize = 32;

#[derive(Clone)]
pub(crate) struct LocalMemoryRuntime {
    provider: Arc<LocalMemoryProvider>,
    project_key: Arc<str>,
    recall: MemoryRecallOptions,
    initializer: LocalMemoryProviderInitializer,
    session_roots: Arc<[PathBuf]>,
}

impl LocalMemoryRuntime {
    pub(crate) fn new(
        provider: Arc<LocalMemoryProvider>,
        cwd: impl AsRef<Path>,
        recall: MemoryRecallOptions,
        initializer: LocalMemoryProviderInitializer,
        session_roots: Vec<PathBuf>,
    ) -> Self {
        Self {
            provider,
            project_key: Arc::from(project_key(cwd.as_ref())),
            recall,
            initializer,
            session_roots: Arc::from(deduplicate_roots(session_roots)),
        }
    }

    pub(crate) fn project_key(&self) -> &str {
        &self.project_key
    }

    pub(crate) fn project_scope(&self) -> MemoryScope {
        MemoryScope::Project {
            root: self.project_key.to_string(),
        }
    }

    pub(crate) fn recall_options(&self) -> MemoryRecallOptions {
        self.recall
    }

    pub(crate) fn scopes(&self, session_id: &str) -> Vec<MemoryScope> {
        vec![
            MemoryScope::User,
            self.project_scope(),
            MemoryScope::Session {
                session_id: session_id.to_string(),
            },
        ]
    }

    pub(crate) async fn recall(&self, query: RecallQuery) -> Result<RecallResult, MemoryError> {
        self.provider.recall(query).await
    }

    pub(crate) async fn apply(
        &self,
        mutations: Vec<MemoryMutation>,
    ) -> Result<ApplyReceipt, MemoryError> {
        self.provider.apply(mutations).await
    }

    pub(crate) async fn search_sessions(
        &self,
        query: SessionSearchQuery,
    ) -> Result<Vec<SessionSearchHit>, MemoryError> {
        self.provider.search_sessions(query).await
    }

    pub(crate) async fn reconcile_snapshot(
        &self,
        snapshot: SessionSnapshot,
    ) -> Result<(), MemoryError> {
        let mutations = snapshot_mutations(&snapshot)?;
        if !mutations.is_empty() {
            self.provider.apply(mutations).await?;
        }
        self.provider
            .replace_session(snapshot_session_document(&snapshot, self.project_key()))
            .await
    }

    pub(crate) async fn health(&self) -> Result<MemoryHealth, MemoryError> {
        self.provider.health().await
    }

    pub(crate) fn embedding_model_status(&self) -> MemoryEmbeddingRuntimeStatus {
        MemoryEmbeddingRuntimeStatus {
            model: self.initializer.model_status(),
            dense_active: self.initializer.dense_active(),
            runtime_error: self.initializer.initialization_issue(),
        }
    }

    pub(crate) async fn install_embedding_model(
        &self,
    ) -> Result<InstalledMemoryEmbedding, MemoryError> {
        let prepared = self.initializer.prepare_embedding_model().await?;
        let mut backfill = MemoryEmbeddingBackfillReceipt::default();
        loop {
            let receipt = prepared
                .backfill_embeddings(EMBEDDING_BACKFILL_BATCH_SIZE)
                .await?;
            let made_progress = receipt.indexed > 0;
            accumulate_backfill(&mut backfill, receipt);
            if backfill.remaining == 0 || !made_progress {
                return Ok(InstalledMemoryEmbedding {
                    install: prepared.install_receipt().clone(),
                    backfill,
                });
            }
        }
    }

    pub(crate) async fn backfill_embeddings(
        &self,
    ) -> Result<MemoryEmbeddingBackfillReceipt, MemoryError> {
        if !self.initializer.dense_active() {
            return Err(MemoryError::Maintenance(
                "dense memory is not active; run `/memory-local-model-install` first".to_string(),
            ));
        }
        let mut backfill = MemoryEmbeddingBackfillReceipt::default();
        loop {
            let receipt = self
                .provider
                .backfill_embeddings(EMBEDDING_BACKFILL_BATCH_SIZE)
                .await?;
            let made_progress = receipt.indexed > 0;
            accumulate_backfill(&mut backfill, receipt);
            if backfill.remaining == 0 || !made_progress {
                return Ok(backfill);
            }
        }
    }

    pub(crate) async fn backfill_embeddings_if_active(
        &self,
    ) -> Result<Option<MemoryEmbeddingBackfillReceipt>, MemoryError> {
        if self.initializer.dense_active() {
            self.backfill_embeddings().await.map(Some)
        } else {
            Ok(None)
        }
    }

    pub(crate) async fn rebuild(&self) -> Result<MemoryRebuildReceipt, MemoryError> {
        let provider = Arc::clone(&self.provider);
        let roots = Arc::clone(&self.session_roots);
        provider.rebuild_with(move || rebuild_batch(&roots)).await
    }
}

fn snapshot_mutations(snapshot: &SessionSnapshot) -> Result<Vec<MemoryMutation>, MemoryError> {
    snapshot
        .entries()
        .iter()
        .filter(|entry| matches!(entry.kind(), SessionEntryKind::Custom))
        .filter(|entry| {
            entry
                .raw()
                .get("customType")
                .and_then(serde_json::Value::as_str)
                == Some(MEMORY_EVENT_TYPE)
        })
        .filter_map(|entry| entry.raw().get("data"))
        .map(|value| {
            serde_json::from_value(value.clone()).map_err(|error| {
                MemoryError::Provider(format!("invalid {MEMORY_EVENT_TYPE} entry: {error}"))
            })
        })
        .collect()
}

fn snapshot_session_document(
    snapshot: &SessionSnapshot,
    project_key: &str,
) -> SessionIndexDocument {
    let entries = snapshot
        .branch()
        .iter()
        .filter_map(session_snapshot_index_entry)
        .collect();
    SessionIndexDocument {
        session_id: snapshot.id().to_string(),
        project_key: project_key.to_string(),
        entries,
    }
}

fn session_snapshot_index_entry(entry: &pi_core::SessionEntryView) -> Option<SessionIndexEntry> {
    if !matches!(entry.kind(), SessionEntryKind::Message) {
        return None;
    }
    let message: Message = serde_json::from_value(entry.raw().get("message")?.clone()).ok()?;
    let (role, text) = message_text(&message)?;
    let text = truncate_utf8(text.trim(), MAX_INDEXED_ENTRY_BYTES).to_string();
    (!text.is_empty()).then(|| SessionIndexEntry {
        entry_id: entry.id().to_string(),
        role: role.to_string(),
        text,
        timestamp_ms: entry.timestamp_ms(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MemoryEmbeddingRuntimeStatus {
    pub model: FastEmbedModelStatus,
    pub dense_active: bool,
    pub runtime_error: Option<String>,
}

pub(crate) struct InstalledMemoryEmbedding {
    pub(crate) install: FastEmbedInstallReceipt,
    pub(crate) backfill: MemoryEmbeddingBackfillReceipt,
}

fn accumulate_backfill(
    total: &mut MemoryEmbeddingBackfillReceipt,
    receipt: MemoryEmbeddingBackfillReceipt,
) {
    total.attempted += receipt.attempted;
    total.indexed += receipt.indexed;
    total.remaining = receipt.remaining;
}

fn deduplicate_roots(roots: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    roots
        .into_iter()
        .filter_map(|root| {
            let normalized = canonical_or_original(&root);
            seen.insert(normalized.clone()).then_some(normalized)
        })
        .collect()
}

fn rebuild_batch(roots: &[PathBuf]) -> Result<MemoryRebuildBatch, MemoryError> {
    let files = discover_session_files(roots)?;
    let mut batch = MemoryRebuildBatch {
        source_files: files.len(),
        ..MemoryRebuildBatch::default()
    };
    let mut session_ids = std::collections::HashMap::new();
    for path in files {
        let format = pi_session::inspect_session_file(&path).map_err(|error| {
            MemoryError::Maintenance(format!(
                "cannot inspect session {}: {error}",
                path.display()
            ))
        })?;
        if matches!(format, SessionFileFormat::Legacy { .. }) {
            batch.skipped_files += 1;
            continue;
        }
        let document = SessionLog::read(&path).map_err(|error| {
            MemoryError::Maintenance(format!("cannot read session {}: {error}", path.display()))
        })?;
        if let Some(previous) = session_ids.insert(document.header.id.clone(), path.clone()) {
            return Err(MemoryError::Maintenance(format!(
                "session id {} appears in both {} and {}; rebuild requires unique v4 session ids",
                document.header.id,
                previous.display(),
                path.display()
            )));
        }
        batch
            .mutations
            .extend(document_mutations(&document.entries, &path)?);
        batch.sessions.push(document_session_index(&document)?);
    }
    Ok(batch)
}

fn discover_session_files(roots: &[PathBuf]) -> Result<Vec<PathBuf>, MemoryError> {
    let mut seen = HashSet::new();
    let mut files = Vec::new();
    for root in roots {
        if !root.exists() {
            continue;
        }
        let entries = std::fs::read_dir(root).map_err(|error| {
            MemoryError::Maintenance(format!(
                "cannot scan session directory {}: {error}",
                root.display()
            ))
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                MemoryError::Maintenance(format!(
                    "cannot scan session directory {}: {error}",
                    root.display()
                ))
            })?;
            let file_type = entry.file_type().map_err(|error| {
                MemoryError::Maintenance(format!(
                    "cannot inspect session candidate {}: {error}",
                    entry.path().display()
                ))
            })?;
            let path = entry.path();
            if !file_type.is_file() || path.extension() != Some(std::ffi::OsStr::new("jsonl")) {
                continue;
            }
            let normalized = canonical_or_original(&path);
            if seen.insert(normalized.clone()) {
                files.push(normalized);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn document_mutations(
    entries: &[SessionRecord],
    path: &Path,
) -> Result<Vec<MemoryMutation>, MemoryError> {
    entries
        .iter()
        .filter_map(|record| match &record.entry {
            SessionEntry::Custom(custom) if custom.custom_type == MEMORY_EVENT_TYPE => {
                Some(custom.data.as_ref())
            }
            _ => None,
        })
        .map(|data| {
            let data = data.ok_or_else(|| {
                MemoryError::Maintenance(format!(
                    "session {} contains a {MEMORY_EVENT_TYPE} entry without data",
                    path.display()
                ))
            })?;
            serde_json::from_value(data.clone()).map_err(|error| {
                MemoryError::Maintenance(format!(
                    "session {} contains an invalid {MEMORY_EVENT_TYPE} entry: {error}",
                    path.display()
                ))
            })
        })
        .collect()
}

fn document_session_index(
    document: &pi_session::SessionDocument,
) -> Result<SessionIndexDocument, MemoryError> {
    let entries = document
        .branch()
        .map_err(|error| {
            MemoryError::Maintenance(format!(
                "cannot project session {}: {error}",
                document.header.id
            ))
        })?
        .into_iter()
        .filter_map(session_record_index_entry)
        .collect();
    Ok(SessionIndexDocument {
        session_id: document.header.id.clone(),
        project_key: project_key(&document.header.cwd),
        entries,
    })
}

fn session_record_index_entry(record: &SessionRecord) -> Option<SessionIndexEntry> {
    let SessionEntry::Message(entry) = &record.entry else {
        return None;
    };
    let (role, text) = message_text(entry.message.as_standard()?)?;
    let text = truncate_utf8(text.trim(), MAX_INDEXED_ENTRY_BYTES).to_string();
    (!text.is_empty()).then(|| SessionIndexEntry {
        entry_id: record.id.clone(),
        role: role.to_string(),
        text,
        timestamp_ms: record.timestamp_ms,
    })
}

fn message_text(message: &Message) -> Option<(&'static str, String)> {
    let (role, blocks) = match message {
        Message::User(message) => ("user", message.content.as_slice()),
        Message::Assistant(message) => ("assistant", message.content.as_slice()),
        Message::ToolResult(_) | Message::Custom(_) => return None,
    };
    let text = blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    Some((role, text))
}

fn project_key(cwd: &Path) -> String {
    let root = cwd
        .ancestors()
        .find(|candidate| candidate.join(".git").exists())
        .unwrap_or(cwd);
    canonical_or_original(root).to_string_lossy().into_owned()
}

fn canonical_or_original(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

pub(crate) fn truncate_utf8(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut boundary = max_bytes;
    while !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    &text[..boundary]
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::{Message, UserMessage};
    use pi_memory_loader::MemoryRecallOptions;
    use pi_session::{SessionHeader, SessionLog};

    use crate::embedding::initialization::LocalMemoryProviderInitializationMode;
    use crate::{
        MemoryEvidence, MemoryKind, MemoryOrigin, MemoryRecord, MemoryScope, RecallQuery,
        SessionSearchQuery,
    };

    #[tokio::test]
    async fn rebuild_scans_every_v4_session_in_the_configured_roots() {
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("project");
        let sessions = directory.path().join("sessions");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        std::fs::create_dir_all(&sessions).unwrap();
        let project_key = std::fs::canonicalize(&project)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let mutation = MemoryMutation::Remember {
            mutation_id: "canonical-mutation".to_string(),
            record: MemoryRecord {
                id: "canonical-record".to_string(),
                scope: MemoryScope::Project {
                    root: project_key.clone(),
                },
                kind: MemoryKind::Preference,
                text: "Prefer Rust examples".to_string(),
                origin: MemoryOrigin {
                    session_id: "session-one".to_string(),
                    entry_id: None,
                    tool_call_id: None,
                },
                evidence: MemoryEvidence {
                    note: "explicit request".to_string(),
                },
                recorded_at_ms: 2,
                supersedes: None,
            },
        };
        let log = SessionLog::create(
            sessions.join("one.jsonl"),
            SessionHeader::new("session-one", &project),
        )
        .unwrap();
        log.append_message(Message::User(UserMessage::text(
            "This transcript is searchable",
            1,
        )))
        .unwrap();
        log.append_custom_entry(
            MEMORY_EVENT_TYPE,
            Some(serde_json::to_value(mutation).unwrap()),
        )
        .unwrap();
        std::fs::write(
            sessions.join("legacy.jsonl"),
            "{\"type\":\"session\",\"version\":3}\n",
        )
        .unwrap();

        let initializer = LocalMemoryProviderInitializer::new(
            directory.path().join("memory.sqlite3"),
            directory.path().join("models"),
            LocalMemoryProviderInitializationMode::Offline,
        );
        let initialized = initializer.initialize().await.unwrap();
        let runtime = LocalMemoryRuntime::new(
            initialized,
            &project,
            MemoryRecallOptions::default(),
            initializer,
            vec![sessions],
        );
        runtime
            .apply(vec![MemoryMutation::Remember {
                mutation_id: "stray-mutation".to_string(),
                record: MemoryRecord {
                    id: "stray-record".to_string(),
                    scope: MemoryScope::Project {
                        root: project_key.clone(),
                    },
                    kind: MemoryKind::Fact,
                    text: "stray derived row".to_string(),
                    origin: MemoryOrigin {
                        session_id: "stray-session".to_string(),
                        entry_id: None,
                        tool_call_id: None,
                    },
                    evidence: MemoryEvidence {
                        note: "test".to_string(),
                    },
                    recorded_at_ms: 1,
                    supersedes: None,
                },
            }])
            .await
            .unwrap();

        let receipt = runtime.rebuild().await.unwrap();

        assert_eq!(receipt.source_files, 2);
        assert_eq!(receipt.skipped_files, 1);
        assert_eq!(receipt.sessions, 1);
        assert_eq!(receipt.session_entries, 1);
        let recalled = runtime
            .recall(RecallQuery {
                text: String::new(),
                scopes: vec![MemoryScope::Project {
                    root: project_key.clone(),
                }],
                limit: 10,
            })
            .await
            .unwrap();
        assert_eq!(recalled.hits.len(), 1);
        assert_eq!(recalled.hits[0].record.id, "canonical-record");
        let searched = runtime
            .search_sessions(SessionSearchQuery {
                text: "searchable".to_string(),
                project_key,
                session_id: None,
                limit: 10,
            })
            .await
            .unwrap();
        assert_eq!(searched[0].session_id, "session-one");
    }
}
