use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{EvalAbility, EvalError, EvalLanguageRelation, ForbiddenReason};

pub const REPORT_SCHEMA_VERSION: u32 = 5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CaseStatus {
    Completed,
    TimedOut,
    BackendError { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForbiddenHit {
    pub record_id: String,
    pub reason: ForbiddenReason,
    pub rank: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CandidateSetCoverage {
    pub record_ids: Vec<String>,
    pub candidate_count: usize,
    pub matched_evidence_hops: usize,
    pub recall: f64,
    pub all_hops: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CandidateCoverage {
    pub sparse: CandidateSetCoverage,
    pub dense: CandidateSetCoverage,
    pub union: CandidateSetCoverage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ranking_stages: Option<RankingStageCoverage>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RankingStageCoverage {
    pub protected_core: CandidateSetCoverage,
    pub gate_eligible: CandidateSetCoverage,
    pub pre_cutoff: CandidateSetCoverage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaseReport {
    pub question_id: String,
    pub ability: EvalAbility,
    pub language: String,
    pub language_relation: EvalLanguageRelation,
    pub status: CaseStatus,
    pub latency_ms: f64,
    pub returned_hit_count: usize,
    pub hit_record_ids: Vec<String>,
    pub hit_scores: Vec<f64>,
    pub matched_evidence_hops: usize,
    pub evidence_hop_count: usize,
    pub recall_at_1: f64,
    pub recall_at_5: f64,
    pub recall_at_8: f64,
    pub all_hops_at_limit: bool,
    pub reciprocal_rank: f64,
    pub evidence_density: f64,
    pub forbidden_hits: Vec<ForbiddenHit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_coverage: Option<CandidateCoverage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HopClass {
    SingleHop,
    MultiHop,
}

impl HopClass {
    pub(crate) const fn from_count(count: usize) -> Self {
        if count > 1 {
            Self::MultiHop
        } else {
            Self::SingleHop
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RetrievalMetrics {
    pub recall_at_1: f64,
    pub recall_at_5: f64,
    pub recall_at_8: f64,
    pub all_hops_rate: f64,
    pub mean_reciprocal_rank: f64,
    pub evidence_density: f64,
    pub wrong_scope_case_rate: f64,
    pub stale_case_rate: f64,
    pub distractor_case_rate: f64,
    pub wrong_scope_hits: usize,
    pub stale_hits: usize,
    pub distractor_hits: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SliceMetrics {
    pub cases: usize,
    pub recall_at_5: f64,
    pub all_hops_rate: f64,
    pub mean_reciprocal_rank: f64,
    pub evidence_density: f64,
    pub forbidden_case_rate: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LatencyMetrics {
    pub mean: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CandidateCoverageMetrics {
    pub cases: usize,
    pub sparse_recall: f64,
    pub sparse_all_hops_rate: f64,
    pub dense_recall: f64,
    pub dense_all_hops_rate: f64,
    pub union_recall: f64,
    pub union_all_hops_rate: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ranking_stages: Option<RankingStageCoverageMetrics>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RankingStageCoverageMetrics {
    pub cases: usize,
    pub protected_core_recall: f64,
    pub protected_core_all_hops_rate: f64,
    pub gate_eligible_recall: f64,
    pub gate_eligible_all_hops_rate: f64,
    pub pre_cutoff_recall: f64,
    pub pre_cutoff_all_hops_rate: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvalSummary {
    pub total_cases: usize,
    pub completed: usize,
    pub timed_out: usize,
    pub backend_errors: usize,
    pub timeout_rate: f64,
    pub retrieval: RetrievalMetrics,
    pub latency_ms: LatencyMetrics,
    pub by_ability: BTreeMap<EvalAbility, SliceMetrics>,
    pub by_language_relation: BTreeMap<EvalLanguageRelation, SliceMetrics>,
    pub by_hop_class: BTreeMap<HopClass, SliceMetrics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_coverage: Option<CandidateCoverageMetrics>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvalReport {
    pub schema_version: u32,
    pub corpus: String,
    pub corpus_version: String,
    pub corpus_seed: u64,
    pub suite: String,
    pub haystack: String,
    pub backend: String,
    pub timeout_ms: u64,
    pub sessions: usize,
    pub records: usize,
    pub summary: EvalSummary,
    pub cases: Vec<CaseReport>,
}

impl EvalReport {
    pub fn write_json(&self, path: impl AsRef<Path>) -> Result<(), EvalError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|source| EvalError::Write {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let file = std::fs::File::create(path).map_err(|source| EvalError::Write {
            path: path.to_path_buf(),
            source,
        })?;
        let mut writer = std::io::BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, self).map_err(|source| EvalError::Json {
            path: path.to_path_buf(),
            line: None,
            source,
        })?;
        writer.write_all(b"\n").map_err(|source| EvalError::Write {
            path: path.to_path_buf(),
            source,
        })?;
        writer.flush().map_err(|source| EvalError::Write {
            path: path.to_path_buf(),
            source,
        })
    }
}
