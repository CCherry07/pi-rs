use std::collections::{BTreeMap, HashMap, HashSet};

use crate::{
    CandidateCoverage, CandidateCoverageMetrics, CandidateSetCoverage, CaseReport, CaseStatus,
    EvalAbility, EvalCandidateTrace, EvalHit, EvalLanguageRelation, EvalQuestion, EvalSummary,
    ForbiddenHit, ForbiddenReason, HopClass, LatencyMetrics, RankingStageCoverage,
    RankingStageCoverageMetrics, RetrievalMetrics, SliceMetrics,
};

pub(crate) fn score_case(
    question: &EvalQuestion,
    status: CaseStatus,
    latency_ms: f64,
    hits: Vec<EvalHit>,
    candidate_trace: Option<EvalCandidateTrace>,
) -> CaseReport {
    let hits = hits.into_iter().take(question.limit).collect::<Vec<_>>();
    let relevant = question
        .evidence_hops
        .iter()
        .flatten()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let forbidden = question
        .forbidden
        .iter()
        .map(|record| (record.record_id.as_str(), record.reason))
        .collect::<HashMap<_, _>>();
    let matched_evidence_hops = question
        .evidence_hops
        .iter()
        .filter(|hop| hop_is_satisfied(hop, &hits, question.limit))
        .count();
    let relevant_hit_count = hits
        .iter()
        .filter(|hit| relevant.contains(hit.record_id.as_str()))
        .count();
    let forbidden_hits = hits
        .iter()
        .enumerate()
        .filter_map(|(index, hit)| {
            forbidden
                .get(hit.record_id.as_str())
                .map(|reason| ForbiddenHit {
                    record_id: hit.record_id.clone(),
                    reason: *reason,
                    rank: index + 1,
                })
        })
        .collect();
    let reciprocal_rank = hits
        .iter()
        .position(|hit| relevant.contains(hit.record_id.as_str()))
        .map_or(0.0, |index| 1.0 / (index + 1) as f64);
    let recall_at_1 = hop_recall(question, &hits, 1);
    let recall_at_5 = hop_recall(question, &hits, 5);
    let recall_at_8 = hop_recall(question, &hits, 8);
    let evidence_density = ratio(relevant_hit_count, hits.len());
    let hit_scores = hits.iter().map(|hit| hit.score).collect();
    let candidate_coverage = candidate_trace.map(|trace| score_candidates(question, trace));

    CaseReport {
        question_id: question.id.clone(),
        ability: question.ability,
        language: question.language.clone(),
        language_relation: question.language_relation,
        status,
        latency_ms,
        returned_hit_count: hits.len(),
        hit_record_ids: hits.into_iter().map(|hit| hit.record_id).collect(),
        hit_scores,
        matched_evidence_hops,
        evidence_hop_count: question.evidence_hops.len(),
        recall_at_1,
        recall_at_5,
        recall_at_8,
        all_hops_at_limit: matched_evidence_hops == question.evidence_hops.len(),
        reciprocal_rank,
        evidence_density,
        forbidden_hits,
        candidate_coverage,
    }
}

fn score_candidates(question: &EvalQuestion, trace: EvalCandidateTrace) -> CandidateCoverage {
    let sparse_record_ids = unique_ids(trace.sparse_record_ids);
    let dense_record_ids = unique_ids(trace.dense_record_ids);
    let union_record_ids = unique_ids(
        sparse_record_ids
            .iter()
            .chain(&dense_record_ids)
            .cloned()
            .collect(),
    );
    CandidateCoverage {
        sparse: score_candidate_set(question, sparse_record_ids),
        dense: score_candidate_set(question, dense_record_ids),
        union: score_candidate_set(question, union_record_ids),
        ranking_stages: trace.ranking_stages.map(|stages| RankingStageCoverage {
            protected_core: score_candidate_set(
                question,
                unique_ids(stages.protected_core_record_ids),
            ),
            gate_eligible: score_candidate_set(
                question,
                unique_ids(stages.gate_eligible_record_ids),
            ),
            pre_cutoff: score_candidate_set(question, unique_ids(stages.pre_cutoff_record_ids)),
        }),
    }
}

fn unique_ids(ids: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    ids.into_iter()
        .filter(|record_id| seen.insert(record_id.clone()))
        .collect()
}

fn score_candidate_set(question: &EvalQuestion, record_ids: Vec<String>) -> CandidateSetCoverage {
    let matched_evidence_hops = question
        .evidence_hops
        .iter()
        .filter(|hop| candidate_hop_is_satisfied(hop, &record_ids))
        .count();
    CandidateSetCoverage {
        candidate_count: record_ids.len(),
        matched_evidence_hops,
        recall: ratio(matched_evidence_hops, question.evidence_hops.len()),
        all_hops: matched_evidence_hops == question.evidence_hops.len(),
        record_ids,
    }
}

fn candidate_hop_is_satisfied(hop: &[String], record_ids: &[String]) -> bool {
    let accepted = hop.iter().map(String::as_str).collect::<HashSet<_>>();
    record_ids
        .iter()
        .any(|record_id| accepted.contains(record_id.as_str()))
}

fn hop_recall(question: &EvalQuestion, hits: &[EvalHit], limit: usize) -> f64 {
    let satisfied = question
        .evidence_hops
        .iter()
        .filter(|hop| hop_is_satisfied(hop, hits, limit))
        .count();
    ratio(satisfied, question.evidence_hops.len())
}

fn hop_is_satisfied(hop: &[String], hits: &[EvalHit], limit: usize) -> bool {
    let accepted = hop.iter().map(String::as_str).collect::<HashSet<_>>();
    hits.iter()
        .take(limit)
        .any(|hit| accepted.contains(hit.record_id.as_str()))
}

pub(crate) fn summarize(cases: &[CaseReport]) -> EvalSummary {
    let total_cases = cases.len();
    let completed = cases
        .iter()
        .filter(|case| matches!(case.status, CaseStatus::Completed))
        .count();
    let timed_out = cases
        .iter()
        .filter(|case| matches!(case.status, CaseStatus::TimedOut))
        .count();
    let backend_errors = cases
        .iter()
        .filter(|case| matches!(case.status, CaseStatus::BackendError { .. }))
        .count();
    let retrieval = retrieval_metrics(cases);
    let latency_ms = latency_metrics(cases);
    let mut grouped = BTreeMap::<EvalAbility, Vec<&CaseReport>>::new();
    for case in cases {
        grouped.entry(case.ability).or_default().push(case);
    }
    let by_ability = grouped
        .into_iter()
        .map(|(ability, cases)| (ability, slice_metrics(&cases)))
        .collect();
    let mut grouped = BTreeMap::<EvalLanguageRelation, Vec<&CaseReport>>::new();
    for case in cases {
        grouped
            .entry(case.language_relation)
            .or_default()
            .push(case);
    }
    let by_language_relation = grouped
        .into_iter()
        .map(|(relation, cases)| (relation, slice_metrics(&cases)))
        .collect();
    let mut grouped = BTreeMap::<HopClass, Vec<&CaseReport>>::new();
    for case in cases {
        grouped
            .entry(HopClass::from_count(case.evidence_hop_count))
            .or_default()
            .push(case);
    }
    let by_hop_class = grouped
        .into_iter()
        .map(|(hop_class, cases)| (hop_class, slice_metrics(&cases)))
        .collect();
    let candidate_coverage = candidate_coverage_metrics(cases);
    EvalSummary {
        total_cases,
        completed,
        timed_out,
        backend_errors,
        timeout_rate: ratio(timed_out, total_cases),
        retrieval,
        latency_ms,
        by_ability,
        by_language_relation,
        by_hop_class,
        candidate_coverage,
    }
}

fn candidate_coverage_metrics(cases: &[CaseReport]) -> Option<CandidateCoverageMetrics> {
    let coverage = cases
        .iter()
        .filter_map(|case| case.candidate_coverage.as_ref())
        .collect::<Vec<_>>();
    let count = coverage.len();
    (count > 0).then(|| CandidateCoverageMetrics {
        cases: count,
        sparse_recall: average(
            coverage.iter().map(|coverage| coverage.sparse.recall),
            count,
        ),
        sparse_all_hops_rate: ratio(
            coverage
                .iter()
                .filter(|coverage| coverage.sparse.all_hops)
                .count(),
            count,
        ),
        dense_recall: average(coverage.iter().map(|coverage| coverage.dense.recall), count),
        dense_all_hops_rate: ratio(
            coverage
                .iter()
                .filter(|coverage| coverage.dense.all_hops)
                .count(),
            count,
        ),
        union_recall: average(coverage.iter().map(|coverage| coverage.union.recall), count),
        union_all_hops_rate: ratio(
            coverage
                .iter()
                .filter(|coverage| coverage.union.all_hops)
                .count(),
            count,
        ),
        ranking_stages: ranking_stage_coverage_metrics(&coverage),
    })
}

fn ranking_stage_coverage_metrics(
    coverage: &[&CandidateCoverage],
) -> Option<RankingStageCoverageMetrics> {
    let stages = coverage
        .iter()
        .filter_map(|coverage| coverage.ranking_stages.as_ref())
        .collect::<Vec<_>>();
    let count = stages.len();
    (count > 0).then(|| RankingStageCoverageMetrics {
        cases: count,
        protected_core_recall: average(
            stages.iter().map(|stages| stages.protected_core.recall),
            count,
        ),
        protected_core_all_hops_rate: ratio(
            stages
                .iter()
                .filter(|stages| stages.protected_core.all_hops)
                .count(),
            count,
        ),
        gate_eligible_recall: average(
            stages.iter().map(|stages| stages.gate_eligible.recall),
            count,
        ),
        gate_eligible_all_hops_rate: ratio(
            stages
                .iter()
                .filter(|stages| stages.gate_eligible.all_hops)
                .count(),
            count,
        ),
        pre_cutoff_recall: average(stages.iter().map(|stages| stages.pre_cutoff.recall), count),
        pre_cutoff_all_hops_rate: ratio(
            stages
                .iter()
                .filter(|stages| stages.pre_cutoff.all_hops)
                .count(),
            count,
        ),
    })
}

fn slice_metrics(cases: &[&CaseReport]) -> SliceMetrics {
    let count = cases.len();
    SliceMetrics {
        cases: count,
        recall_at_5: average(cases.iter().map(|case| case.recall_at_5), count),
        all_hops_rate: ratio(
            cases.iter().filter(|case| case.all_hops_at_limit).count(),
            count,
        ),
        mean_reciprocal_rank: average(cases.iter().map(|case| case.reciprocal_rank), count),
        evidence_density: average(cases.iter().map(|case| case.evidence_density), count),
        forbidden_case_rate: ratio(
            cases
                .iter()
                .filter(|case| !case.forbidden_hits.is_empty())
                .count(),
            count,
        ),
    }
}

fn retrieval_metrics(cases: &[CaseReport]) -> RetrievalMetrics {
    let count = cases.len();
    let wrong_scope_hits = count_forbidden_hits(cases, ForbiddenReason::WrongScope);
    let stale_hits = count_forbidden_hits(cases, ForbiddenReason::Stale);
    let distractor_hits = count_forbidden_hits(cases, ForbiddenReason::Distractor);
    RetrievalMetrics {
        recall_at_1: average(cases.iter().map(|case| case.recall_at_1), count),
        recall_at_5: average(cases.iter().map(|case| case.recall_at_5), count),
        recall_at_8: average(cases.iter().map(|case| case.recall_at_8), count),
        all_hops_rate: ratio(
            cases.iter().filter(|case| case.all_hops_at_limit).count(),
            count,
        ),
        mean_reciprocal_rank: average(cases.iter().map(|case| case.reciprocal_rank), count),
        evidence_density: average(cases.iter().map(|case| case.evidence_density), count),
        wrong_scope_case_rate: forbidden_case_rate(cases, ForbiddenReason::WrongScope),
        stale_case_rate: forbidden_case_rate(cases, ForbiddenReason::Stale),
        distractor_case_rate: forbidden_case_rate(cases, ForbiddenReason::Distractor),
        wrong_scope_hits,
        stale_hits,
        distractor_hits,
    }
}

fn latency_metrics(cases: &[CaseReport]) -> LatencyMetrics {
    let mut values = cases.iter().map(|case| case.latency_ms).collect::<Vec<_>>();
    values.sort_by(f64::total_cmp);
    LatencyMetrics {
        mean: average(values.iter().copied(), values.len()),
        p50: percentile(&values, 0.50),
        p95: percentile(&values, 0.95),
        p99: percentile(&values, 0.99),
        max: values.last().copied().unwrap_or_default(),
    }
}

fn percentile(sorted: &[f64], quantile: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (quantile * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn count_forbidden_hits(cases: &[CaseReport], reason: ForbiddenReason) -> usize {
    cases
        .iter()
        .flat_map(|case| &case.forbidden_hits)
        .filter(|hit| hit.reason == reason)
        .count()
}

fn forbidden_case_rate(cases: &[CaseReport], reason: ForbiddenReason) -> f64 {
    ratio(
        cases
            .iter()
            .filter(|case| case.forbidden_hits.iter().any(|hit| hit.reason == reason))
            .count(),
        cases.len(),
    )
}

fn average(values: impl Iterator<Item = f64>, count: usize) -> f64 {
    if count == 0 {
        0.0
    } else {
        values.sum::<f64>() / count as f64
    }
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_percentiles_are_stable() {
        let values = [1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(percentile(&values, 0.50), 3.0);
        assert_eq!(percentile(&values, 0.95), 5.0);
    }
}
