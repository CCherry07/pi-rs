use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use pi_plugin_memory_local::{LocalMemoryProvider, MemoryScope, RecallQuery};
use serde::{Deserialize, Serialize};

use crate::{EvalQuestion, EvalSuite};

/// The complete input visible to a backend under test.
///
/// Question ids, abilities, gold evidence, forbidden records, and expected
/// answers intentionally cannot cross this seam.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvalInput {
    pub query: String,
    pub scopes: Vec<MemoryScope>,
    pub limit: usize,
}

impl EvalInput {
    pub(crate) fn from_question(question: &EvalQuestion) -> Self {
        Self {
            query: question.query.clone(),
            scopes: question.scopes.clone(),
            limit: question.limit,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvalHit {
    pub record_id: String,
    pub text: String,
    pub score: f64,
}

/// Rank-ordered candidate identities observed before final ranking.
///
/// The backend never receives gold evidence. The runner compares these ids
/// with its private gold metadata after the backend returns.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvalCandidateTrace {
    pub sparse_record_ids: Vec<String>,
    pub dense_record_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ranking_stages: Option<EvalRankingStageTrace>,
}

/// Candidate identities at product hybrid-ranking boundaries.
///
/// These stages are intentionally unavailable on generic providers and raw
/// control backends. They describe the concrete SQLite hybrid Adapter only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvalRankingStageTrace {
    pub protected_core_record_ids: Vec<String>,
    pub gate_eligible_record_ids: Vec<String>,
    pub pre_cutoff_record_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvalObservation {
    pub hits: Vec<EvalHit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_trace: Option<EvalCandidateTrace>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct EvalBackendError {
    message: String,
}

impl EvalBackendError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[async_trait]
pub trait EvalBackend: Send + Sync {
    async fn gather(&self, input: EvalInput) -> Result<EvalObservation, EvalBackendError>;
}

/// Adapter from local recall to the evaluation seam.
pub struct ProviderBackend {
    provider: Arc<LocalMemoryProvider>,
}

impl ProviderBackend {
    pub fn new(provider: LocalMemoryProvider) -> Self {
        Self {
            provider: Arc::new(provider),
        }
    }

    pub fn shared(provider: Arc<LocalMemoryProvider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl EvalBackend for ProviderBackend {
    async fn gather(&self, input: EvalInput) -> Result<EvalObservation, EvalBackendError> {
        let result = self
            .provider
            .recall(RecallQuery {
                text: input.query,
                scopes: input.scopes,
                limit: input.limit,
            })
            .await
            .map_err(|error| EvalBackendError::new(error.to_string()))?;
        Ok(EvalObservation {
            hits: result
                .hits
                .into_iter()
                .map(|hit| EvalHit {
                    record_id: hit.record.id,
                    text: hit.record.text,
                    score: hit.score,
                })
                .collect(),
            candidate_trace: None,
        })
    }
}

/// Evaluation Adapter for SQLite-specific pre-ranking candidate diagnostics.
///
/// This stays separate from [`ProviderBackend`] because candidate tracing is a
/// concrete evaluation capability rather than ordinary recall output.
pub struct SqliteProviderBackend {
    provider: LocalMemoryProvider,
}

impl SqliteProviderBackend {
    pub fn new(provider: LocalMemoryProvider) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl EvalBackend for SqliteProviderBackend {
    async fn gather(&self, input: EvalInput) -> Result<EvalObservation, EvalBackendError> {
        let traced = self
            .provider
            .recall_with_candidates(RecallQuery {
                text: input.query,
                scopes: input.scopes,
                limit: input.limit,
            })
            .await
            .map_err(|error| EvalBackendError::new(error.to_string()))?;
        Ok(EvalObservation {
            hits: traced
                .result
                .hits
                .into_iter()
                .map(|hit| EvalHit {
                    record_id: hit.record.id,
                    text: hit.record.text,
                    score: hit.score,
                })
                .collect(),
            candidate_trace: Some(EvalCandidateTrace {
                sparse_record_ids: traced.sparse_record_ids,
                dense_record_ids: traced.dense_record_ids,
                ranking_stages: traced.ranking_stages.map(|stages| EvalRankingStageTrace {
                    protected_core_record_ids: stages.protected_core_record_ids,
                    gate_eligible_record_ids: stages.gate_eligible_record_ids,
                    pre_cutoff_record_ids: stages.pre_cutoff_record_ids,
                }),
            }),
        })
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NoRecallBackend;

#[async_trait]
impl EvalBackend for NoRecallBackend {
    async fn gather(&self, _input: EvalInput) -> Result<EvalObservation, EvalBackendError> {
        Ok(EvalObservation::default())
    }
}

/// A deliberately privileged upper-bound Adapter.
///
/// It is built from gold evidence but still receives the same redacted input at
/// query time. Never use this as a product backend or a privacy regression.
#[derive(Debug, Clone)]
pub struct OracleBackend {
    observations: HashMap<EvalInput, EvalObservation>,
}

impl OracleBackend {
    pub fn new(suite: &EvalSuite) -> Self {
        let observations = suite
            .questions()
            .iter()
            .map(|question| {
                let mut seen = HashSet::new();
                let hits = question
                    .evidence_hops
                    .iter()
                    .filter_map(|hop| hop.first())
                    .filter(|record_id| seen.insert(record_id.as_str()))
                    .enumerate()
                    .map(|(index, record_id)| {
                        let record = suite
                            .record(record_id)
                            .expect("validated oracle evidence record");
                        EvalHit {
                            record_id: record.id.clone(),
                            text: record.text.clone(),
                            score: 1.0 / (index + 1) as f64,
                        }
                    })
                    .collect();
                (
                    EvalInput::from_question(question),
                    EvalObservation {
                        hits,
                        candidate_trace: None,
                    },
                )
            })
            .collect();
        Self { observations }
    }
}

#[async_trait]
impl EvalBackend for OracleBackend {
    async fn gather(&self, input: EvalInput) -> Result<EvalObservation, EvalBackendError> {
        self.observations.get(&input).cloned().ok_or_else(|| {
            EvalBackendError::new("oracle received an input outside the validated suite")
        })
    }
}
