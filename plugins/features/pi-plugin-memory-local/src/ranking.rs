use std::collections::{BTreeMap, BTreeSet};

use crate::MemoryHit;

const CANDIDATE_MULTIPLIER: usize = 8;
const MIN_CANDIDATES: usize = 32;
const MAX_CANDIDATES: usize = 100;
const MAX_SATURATED_DENSE_CANDIDATES: usize = 256;
const COMPLEX_QUERY_SUBSTANTIVE_TERMS: usize = 12;
const MAX_COMPLEX_QUERY_LEXICAL_CORE: usize = 1;
const QUERY_FACET_TERMS: usize = 8;
const QUERY_FACET_STRIDE_TERMS: usize = 6;
const MIN_QUERY_FACET_SUBSTANTIVE_TERMS: usize = 3;
const MAX_QUERY_VARIANTS: usize = 5;
const FACET_COVERAGE_QUOTA: usize = 2;
const MAX_FACET_REPRESENTATIVE_JACCARD: f64 = 0.8;
const MAX_QUERY_TERMS: usize = 48;
const MIN_RELATIVE_SCORE: f64 = 0.35;
const MIN_SUBSTANTIVE_TERM_CHARS: usize = 4;
const MIN_REPEATED_EVIDENCE_TERMS: usize = 3;
const LEXICAL_WEIGHT_TOTAL: f64 = 0.92;
const DENSE_WEIGHT: f64 = 0.16;
const RRF_WEIGHT: f64 = 0.08;
const HYBRID_WEIGHT_TOTAL: f64 = LEXICAL_WEIGHT_TOTAL + DENSE_WEIGHT + RRF_WEIGHT;
const RRF_K: f64 = 60.0;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct SparseDenseRankingStages {
    pub protected_core_record_ids: Vec<String>,
    pub gate_eligible_record_ids: Vec<String>,
    pub pre_cutoff_record_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SemanticFacetMatch {
    pub(super) best_rank: usize,
    pub(super) primary_mask: u64,
}

pub(super) fn candidate_limit(result_limit: usize) -> usize {
    result_limit
        .saturating_mul(CANDIDATE_MULTIPLIER)
        .clamp(MIN_CANDIDATES, MAX_CANDIDATES)
}

pub(super) fn product_candidate_limit(query: &str, result_limit: usize) -> usize {
    if is_complex_query(query) {
        MAX_CANDIDATES
    } else {
        candidate_limit(result_limit)
    }
}

pub(super) fn product_dense_candidate_limit(
    query: &str,
    primary_limit: usize,
    sparse_count: usize,
) -> usize {
    if is_complex_query(query) && (sparse_count == 0 || sparse_count == primary_limit) {
        MAX_SATURATED_DENSE_CANDIDATES
    } else {
        primary_limit
    }
}

fn is_complex_query(query: &str) -> bool {
    terms(query)
        .into_iter()
        .filter(|term| is_substantive_term(term))
        .collect::<BTreeSet<_>>()
        .len()
        >= COMPLEX_QUERY_SUBSTANTIVE_TERMS
}

pub(super) fn semantic_query_variants(query: &str) -> Vec<String> {
    let query = query.trim();
    let mut variants = vec![query.to_string()];
    if !is_complex_query(query) {
        return variants;
    }
    for (index, boundary) in query
        .char_indices()
        .filter(|(_, character)| is_semantic_boundary(*character))
    {
        let boundary_end = index + boundary.len_utf8();
        for fragment in [&query[..index], &query[boundary_end..]] {
            push_semantic_variant(&mut variants, fragment);
            if variants.len() == MAX_QUERY_VARIANTS {
                return variants;
            }
        }
    }
    for fragment in query.split(is_semantic_boundary) {
        push_semantic_variant(&mut variants, fragment);
        if variants.len() == MAX_QUERY_VARIANTS {
            return variants;
        }
    }
    let query_terms = terms(query);
    let mut starts = (0..query_terms.len())
        .step_by(QUERY_FACET_STRIDE_TERMS)
        .collect::<Vec<_>>();
    starts.push(query_terms.len().saturating_sub(QUERY_FACET_TERMS));
    starts.sort_unstable();
    starts.dedup();
    for start in starts {
        let end = (start + QUERY_FACET_TERMS).min(query_terms.len());
        let facet = join_semantic_terms(&query_terms[start..end]);
        if !facet.is_empty() && !variants.contains(&facet) {
            variants.push(facet);
        }
        if variants.len() == MAX_QUERY_VARIANTS {
            break;
        }
    }
    variants
}

fn push_semantic_variant(variants: &mut Vec<String>, fragment: &str) {
    let fragment = fragment.trim();
    if fragment.is_empty() {
        return;
    }
    let fragment_terms = terms(fragment);
    if fragment_terms
        .iter()
        .filter(|term| is_substantive_term(term))
        .collect::<BTreeSet<_>>()
        .len()
        < MIN_QUERY_FACET_SUBSTANTIVE_TERMS
        || variants
            .iter()
            .any(|variant| terms(variant) == fragment_terms)
    {
        return;
    }
    variants.push(fragment.to_string());
}

fn is_semantic_boundary(character: char) -> bool {
    matches!(
        character,
        ':' | '：' | ',' | '，' | ';' | '；' | '.' | '。' | '?' | '？' | '!' | '！'
    )
}

fn join_semantic_terms(terms: &[String]) -> String {
    let mut text = String::new();
    let mut previous_was_cjk = false;
    for term in terms {
        let is_cjk_term = term.chars().all(is_cjk);
        if !text.is_empty() && !(previous_was_cjk && is_cjk_term) {
            text.push(' ');
        }
        text.push_str(term);
        previous_was_cjk = is_cjk_term;
    }
    text
}

/// Historical equal-weight RRF control retained for evaluation ablations.
/// It intentionally has no confidence or diversity policy.
pub(super) fn fuse_raw_rrf(
    sparse: Vec<MemoryHit>,
    dense: Vec<MemoryHit>,
    result_limit: usize,
) -> Vec<MemoryHit> {
    if result_limit == 0 {
        return Vec::new();
    }

    struct FusedCandidate {
        hit: MemoryHit,
        score: f64,
        best_rank: usize,
    }

    let mut candidates = BTreeMap::<String, FusedCandidate>::new();
    for ranked in [sparse, dense] {
        for (rank, hit) in ranked.into_iter().enumerate() {
            let contribution = 1.0 / (RRF_K + (rank + 1) as f64);
            let candidate =
                candidates
                    .entry(hit.record.id.clone())
                    .or_insert_with(|| FusedCandidate {
                        hit,
                        score: 0.0,
                        best_rank: rank,
                    });
            candidate.score += contribution;
            candidate.best_rank = candidate.best_rank.min(rank);
        }
    }
    let max_score = 2.0 / (RRF_K + 1.0);
    let mut candidates = candidates.into_values().collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.best_rank.cmp(&right.best_rank))
            .then_with(|| {
                right
                    .hit
                    .record
                    .recorded_at_ms
                    .cmp(&left.hit.record.recorded_at_ms)
            })
            .then_with(|| left.hit.record.id.cmp(&right.hit.record.id))
    });
    candidates
        .into_iter()
        .take(result_limit)
        .map(|candidate| MemoryHit {
            score: candidate.score / max_score,
            ..candidate.hit
        })
        .collect()
}

/// Preserve the confidence-filtered lexical core, then admit complementary
/// dense candidates through seeded diversity and a relative-cutoff policy.
/// New evidence deliberately selected by diversity survives that score-only
/// cutoff; coherent phrase/code evidence is prioritized within that pool.
///
/// BM25 and cosine scores are not mixed directly. Sparse rank remains a
/// lexical signal, normalized cosine similarity is a semantic signal, and RRF
/// contributes only bounded cross-retriever agreement.
pub(super) fn fuse_sparse_dense(
    query: &str,
    sparse: Vec<MemoryHit>,
    dense: Vec<MemoryHit>,
    result_limit: usize,
) -> Vec<MemoryHit> {
    fuse_sparse_dense_internal(query, sparse, dense, &BTreeMap::new(), result_limit, None)
}

pub(super) fn fuse_sparse_dense_with_facets(
    query: &str,
    sparse: Vec<MemoryHit>,
    dense: Vec<MemoryHit>,
    facet_matches: &BTreeMap<String, SemanticFacetMatch>,
    result_limit: usize,
) -> Vec<MemoryHit> {
    if facet_matches.is_empty() {
        return fuse_sparse_dense(query, sparse, dense, result_limit);
    }
    fuse_sparse_dense_internal(query, sparse, dense, facet_matches, result_limit, None)
}

pub(super) fn fuse_sparse_dense_with_stages(
    query: &str,
    sparse: Vec<MemoryHit>,
    dense: Vec<MemoryHit>,
    facet_matches: &BTreeMap<String, SemanticFacetMatch>,
    result_limit: usize,
) -> (Vec<MemoryHit>, SparseDenseRankingStages) {
    let mut stages = SparseDenseRankingStages::default();
    let hits = fuse_sparse_dense_internal(
        query,
        sparse,
        dense,
        facet_matches,
        result_limit,
        Some(&mut stages),
    );
    (hits, stages)
}

fn fuse_sparse_dense_internal(
    query: &str,
    sparse: Vec<MemoryHit>,
    dense: Vec<MemoryHit>,
    facet_matches: &BTreeMap<String, SemanticFacetMatch>,
    result_limit: usize,
    mut stages: Option<&mut SparseDenseRankingStages>,
) -> Vec<MemoryHit> {
    if result_limit == 0 {
        return Vec::new();
    }
    let lexical_core = rerank(query, sparse.clone(), result_limit);
    if dense.is_empty() {
        capture_lexical_only_stages(stages, &lexical_core);
        return lexical_core;
    }

    let query_terms = terms(query)
        .into_iter()
        .take(MAX_QUERY_TERMS)
        .collect::<Vec<_>>();
    if query_terms.is_empty() {
        capture_lexical_only_stages(stages, &lexical_core);
        return lexical_core;
    }

    let candidates = fusion_candidates(sparse, dense, facet_matches);
    let query_unique = query_terms.iter().cloned().collect::<BTreeSet<_>>();
    let query_code_atoms = code_atoms(query);
    let term_weights = inverse_document_weights(&query_unique, &candidates);
    let mut scored = score_candidates(
        &query_terms,
        &query_unique,
        &query_code_atoms,
        candidates,
        &term_weights,
        ScoreMode::SparseDense,
    );
    let core_order = lexical_core
        .iter()
        .enumerate()
        .map(|(index, hit)| (hit.record.id.as_str(), index))
        .collect::<BTreeMap<_, _>>();
    let (mut protected, mut remaining): (Vec<_>, Vec<_>) = scored
        .drain(..)
        .partition(|candidate| core_order.contains_key(candidate.hit.record.id.as_str()));
    protected.sort_by_key(|candidate| core_order[&candidate.hit.record.id.as_str()]);
    let complex_query = is_complex_query(query);
    if complex_query && protected.len() > MAX_COMPLEX_QUERY_LEXICAL_CORE {
        remaining.extend(protected.split_off(MAX_COMPLEX_QUERY_LEXICAL_CORE));
    }
    if let Some(stages) = stages.as_deref_mut() {
        stages.protected_core_record_ids = candidate_ids(protected.iter());
    }
    if protected.len() >= result_limit {
        protected.truncate(result_limit);
        if let Some(stages) = stages.as_deref_mut() {
            let record_ids = candidate_ids(protected.iter());
            stages.gate_eligible_record_ids = record_ids.clone();
            stages.pre_cutoff_record_ids = record_ids;
        }
        return into_hits(protected);
    }
    let covered = protected
        .iter()
        .flat_map(|candidate| candidate.matched_terms.iter().cloned())
        .collect::<BTreeSet<_>>();
    for candidate in &mut remaining {
        candidate.admitted_by_semantic_gate = complex_query
            && candidate
                .facet_rank
                .is_some_and(|rank| rank < result_limit.saturating_mul(2));
        if !protected.is_empty() {
            candidate.adds_new_query_evidence =
                adds_new_substantive_query_evidence(candidate, &covered);
            candidate.admitted_by_evidence_gate = adds_distinct_query_evidence(candidate, &covered)
                || candidate.admitted_by_semantic_gate;
        }
    }
    if !protected.is_empty() {
        remaining.retain(|candidate| candidate.admitted_by_evidence_gate);
    }
    if let Some(stages) = stages.as_deref_mut() {
        stages.gate_eligible_record_ids = candidate_ids(protected.iter().chain(remaining.iter()));
    }

    let rescue_limit = result_limit - protected.len();
    let mut rescues = diversify_with_seed(
        remaining,
        &term_weights,
        rescue_limit,
        protected.iter(),
        complex_query,
    );
    if complex_query {
        rescues.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.original_rank.cmp(&right.original_rank))
                .then_with(|| left.hit.record.id.cmp(&right.hit.record.id))
        });
    }
    if let Some(stages) = stages {
        stages.pre_cutoff_record_ids = candidate_ids(protected.iter().chain(rescues.iter()));
    }
    let top_score = protected
        .iter()
        .map(|candidate| candidate.score)
        .chain(rescues.iter().map(|candidate| candidate.score))
        .fold(0.0, f64::max);
    let cutoff = top_score * MIN_RELATIVE_SCORE;
    rescues.retain(|candidate| candidate.admitted_by_evidence_gate || candidate.score >= cutoff);
    protected.extend(rescues);
    into_hits(protected)
}

fn capture_lexical_only_stages(
    stages: Option<&mut SparseDenseRankingStages>,
    lexical_core: &[MemoryHit],
) {
    if let Some(stages) = stages {
        let record_ids = lexical_core
            .iter()
            .map(|hit| hit.record.id.clone())
            .collect::<Vec<_>>();
        stages.protected_core_record_ids = record_ids.clone();
        stages.gate_eligible_record_ids = record_ids.clone();
        stages.pre_cutoff_record_ids = record_ids;
    }
}

fn candidate_ids<'a>(candidates: impl Iterator<Item = &'a ScoredCandidate>) -> Vec<String> {
    candidates
        .map(|candidate| candidate.hit.record.id.clone())
        .collect()
}

pub(super) fn rerank(query: &str, hits: Vec<MemoryHit>, result_limit: usize) -> Vec<MemoryHit> {
    if result_limit == 0 {
        return Vec::new();
    }
    if hits.len() == 1 {
        return hits
            .into_iter()
            .map(|hit| MemoryHit { score: 1.0, ..hit })
            .collect();
    }

    let query_terms = terms(query)
        .into_iter()
        .take(MAX_QUERY_TERMS)
        .collect::<Vec<_>>();
    if query_terms.is_empty() {
        return hits.into_iter().take(result_limit).collect();
    }
    let query_unique = query_terms.iter().cloned().collect::<BTreeSet<_>>();
    let query_code_atoms = code_atoms(query);
    let candidates = hits
        .into_iter()
        .enumerate()
        .map(|(rank, hit)| Candidate::sparse(rank, hit))
        .collect::<Vec<_>>();
    let term_weights = inverse_document_weights(&query_unique, &candidates);
    let mut scored = score_candidates(
        &query_terms,
        &query_unique,
        &query_code_atoms,
        candidates,
        &term_weights,
        ScoreMode::Lexical,
    );
    scored.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.original_rank.cmp(&right.original_rank))
    });
    let mut ranked = diversify(scored, &term_weights, result_limit);
    if let Some(top_score) = ranked.first().map(|candidate| candidate.score) {
        let cutoff = top_score * MIN_RELATIVE_SCORE;
        ranked.retain(|candidate| candidate.score >= cutoff);
    }
    into_hits(ranked)
}

struct Candidate {
    original_rank: usize,
    sparse_rank: Option<usize>,
    dense_rank: Option<usize>,
    dense_similarity: Option<f64>,
    hit: MemoryHit,
    terms: Vec<String>,
    unique_terms: BTreeSet<String>,
    code_atoms: BTreeSet<String>,
    facet_rank: Option<usize>,
    facet_primary_mask: u64,
}

impl Candidate {
    fn sparse(rank: usize, hit: MemoryHit) -> Self {
        Self::new(rank, Some(rank), None, None, None, hit)
    }

    fn new(
        original_rank: usize,
        sparse_rank: Option<usize>,
        dense_rank: Option<usize>,
        dense_similarity: Option<f64>,
        facet_match: Option<SemanticFacetMatch>,
        hit: MemoryHit,
    ) -> Self {
        let record_terms = terms(&hit.record.text);
        let code_atoms = code_atoms(&hit.record.text);
        Self {
            original_rank,
            sparse_rank,
            dense_rank,
            dense_similarity,
            unique_terms: record_terms.iter().cloned().collect(),
            terms: record_terms,
            code_atoms,
            facet_rank: facet_match.map(|facet_match| facet_match.best_rank),
            facet_primary_mask: facet_match.map_or(0, |facet_match| facet_match.primary_mask),
            hit,
        }
    }
}

#[derive(Clone, Copy)]
enum ScoreMode {
    Lexical,
    SparseDense,
}

struct ScoredCandidate {
    original_rank: usize,
    score: f64,
    hit: MemoryHit,
    matched_terms: BTreeSet<String>,
    all_terms: BTreeSet<String>,
    adjacency: f64,
    span: f64,
    code: f64,
    facet_rank: Option<usize>,
    facet_primary_mask: u64,
    admitted_by_evidence_gate: bool,
    admitted_by_semantic_gate: bool,
    adds_new_query_evidence: bool,
}

fn diversify(
    candidates: Vec<ScoredCandidate>,
    term_weights: &BTreeMap<String, f64>,
    result_limit: usize,
) -> Vec<ScoredCandidate> {
    diversify_with_seed(
        candidates,
        term_weights,
        result_limit,
        std::iter::empty(),
        false,
    )
}

fn diversify_with_seed<'a>(
    mut candidates: Vec<ScoredCandidate>,
    term_weights: &BTreeMap<String, f64>,
    result_limit: usize,
    seed: impl Iterator<Item = &'a ScoredCandidate>,
    prioritize_new_evidence: bool,
) -> Vec<ScoredCandidate> {
    let total_query_weight = term_weights.values().sum::<f64>();
    let mut covered = BTreeSet::new();
    let mut facet_coverage = [0_usize; u64::BITS as usize];
    let mut facet_term_sets: [Vec<BTreeSet<String>>; u64::BITS as usize] =
        std::array::from_fn(|_| Vec::new());
    let mut selected_term_sets = Vec::new();
    for candidate in seed {
        covered.extend(candidate.matched_terms.iter().cloned());
        selected_term_sets.push(candidate.all_terms.clone());
    }
    let mut selected = Vec::with_capacity(result_limit.min(candidates.len()));
    while !candidates.is_empty() && selected.len() < result_limit {
        let mut best = None::<(usize, bool, bool, usize, f64, f64, f64, f64, usize)>;
        for (index, candidate) in candidates.iter().enumerate() {
            let selection_score = if selected_term_sets.is_empty() || total_query_weight == 0.0 {
                candidate.score
            } else {
                let uncovered = candidate
                    .matched_terms
                    .difference(&covered)
                    .map(|term| term_weights.get(term).copied().unwrap_or(1.0))
                    .sum::<f64>()
                    / total_query_weight;
                let redundancy = if candidate.matched_terms.is_empty() {
                    0.0
                } else {
                    candidate.matched_terms.intersection(&covered).count() as f64
                        / candidate.matched_terms.len() as f64
                };
                let similarity = selected_term_sets
                    .iter()
                    .map(|selected| jaccard_similarity(&candidate.all_terms, selected))
                    .fold(0.0, f64::max);
                candidate.score + uncovered * 0.30 - redundancy * 0.04 - similarity * 0.25
            };
            let primary_facet = (candidate.facet_primary_mask != 0)
                .then(|| candidate.facet_primary_mask.trailing_zeros() as usize);
            let duplicates_facet_evidence = primary_facet.is_some_and(|facet| {
                facet_term_sets[facet].iter().any(|selected| {
                    jaccard_similarity(&candidate.all_terms, selected)
                        >= MAX_FACET_REPRESENTATIVE_JACCARD
                })
            });
            let adds_new_facet = candidate.admitted_by_semantic_gate
                && !duplicates_facet_evidence
                && primary_facet.is_some_and(|facet| facet_coverage[facet] < FACET_COVERAGE_QUOTA);
            let selected_for_coherent_new_evidence = if prioritize_new_evidence {
                adds_new_facet
            } else {
                candidate.adds_new_query_evidence
            };
            let (adjacency, span, code) = if selected_for_coherent_new_evidence {
                (candidate.adjacency, candidate.span, candidate.code)
            } else {
                (0.0, 0.0, 0.0)
            };
            let facet_rank = if adds_new_facet {
                candidate.facet_rank.unwrap_or(usize::MAX)
            } else {
                usize::MAX
            };
            if best.is_none_or(
                |(
                    _,
                    best_adds_new_evidence,
                    best_adds_new_facet,
                    best_facet_rank,
                    best_adjacency,
                    best_span,
                    best_code,
                    best_score,
                    best_rank,
                )| {
                    if selected_for_coherent_new_evidence != best_adds_new_evidence {
                        selected_for_coherent_new_evidence
                    } else if adds_new_facet != best_adds_new_facet {
                        adds_new_facet
                    } else if facet_rank != best_facet_rank {
                        facet_rank < best_facet_rank
                    } else if adjacency != best_adjacency {
                        adjacency > best_adjacency
                    } else if span != best_span {
                        span > best_span
                    } else if code != best_code {
                        code > best_code
                    } else if selection_score != best_score {
                        selection_score > best_score
                    } else {
                        candidate.original_rank < best_rank
                    }
                },
            ) {
                best = Some((
                    index,
                    selected_for_coherent_new_evidence,
                    adds_new_facet,
                    facet_rank,
                    adjacency,
                    span,
                    code,
                    selection_score,
                    candidate.original_rank,
                ));
            }
        }
        let (best_index, _, adds_new_facet, _, _, _, _, selection_score, _) =
            best.expect("non-empty candidate set");
        let mut candidate = candidates.swap_remove(best_index);
        candidate.score = selection_score;
        covered.extend(candidate.matched_terms.iter().cloned());
        if adds_new_facet && candidate.facet_primary_mask != 0 {
            let facet = candidate.facet_primary_mask.trailing_zeros() as usize;
            facet_coverage[facet] += 1;
            facet_term_sets[facet].push(candidate.all_terms.clone());
        }
        selected_term_sets.push(candidate.all_terms.clone());
        selected.push(candidate);
    }
    selected
}

fn fusion_candidates(
    sparse: Vec<MemoryHit>,
    dense: Vec<MemoryHit>,
    facet_matches: &BTreeMap<String, SemanticFacetMatch>,
) -> Vec<Candidate> {
    struct Sources {
        hit: MemoryHit,
        sparse_rank: Option<usize>,
        dense_rank: Option<usize>,
        dense_similarity: Option<f64>,
    }

    let mut candidates = BTreeMap::<String, Sources>::new();
    for (rank, hit) in sparse.into_iter().enumerate() {
        candidates.insert(
            hit.record.id.clone(),
            Sources {
                hit,
                sparse_rank: Some(rank),
                dense_rank: None,
                dense_similarity: None,
            },
        );
    }
    for (rank, hit) in dense.into_iter().enumerate() {
        let similarity = (1.0 + hit.score).clamp(0.0, 1.0);
        let candidate = candidates.entry(hit.record.id.clone()).or_insert(Sources {
            hit,
            sparse_rank: None,
            dense_rank: Some(rank),
            dense_similarity: Some(similarity),
        });
        candidate.dense_rank = Some(rank);
        candidate.dense_similarity = Some(similarity);
    }
    candidates
        .into_values()
        .map(|candidate| {
            let original_rank = candidate
                .sparse_rank
                .into_iter()
                .chain(candidate.dense_rank)
                .min()
                .expect("fusion candidate has at least one source rank");
            Candidate::new(
                original_rank,
                candidate.sparse_rank,
                candidate.dense_rank,
                candidate.dense_similarity,
                facet_matches.get(&candidate.hit.record.id).copied(),
                candidate.hit,
            )
        })
        .collect()
}

fn score_candidates(
    query_terms: &[String],
    query_unique: &BTreeSet<String>,
    query_code_atoms: &BTreeSet<String>,
    candidates: Vec<Candidate>,
    term_weights: &BTreeMap<String, f64>,
    mode: ScoreMode,
) -> Vec<ScoredCandidate> {
    candidates
        .into_iter()
        .map(|candidate| {
            let coverage = weighted_coverage(query_unique, &candidate.unique_terms, term_weights);
            let matched_terms = query_unique
                .intersection(&candidate.unique_terms)
                .cloned()
                .collect();
            let adjacency = adjacent_pair_coverage(query_terms, &candidate.terms);
            let span = contiguous_span_coverage(query_terms, &candidate.terms);
            let code = set_coverage(query_code_atoms, &candidate.code_atoms);
            let sparse = candidate
                .sparse_rank
                .map_or(0.0, |rank| 1.0 / (rank + 1) as f64);
            let lexical =
                coverage * 0.35 + adjacency * 0.18 + span * 0.10 + code * 0.07 + sparse * 0.22;
            let (extra, weight_total) = match mode {
                ScoreMode::Lexical => (0.0, LEXICAL_WEIGHT_TOTAL),
                ScoreMode::SparseDense => {
                    let dense = candidate.dense_similarity.unwrap_or(0.0);
                    let rrf = normalized_rrf(candidate.sparse_rank, candidate.dense_rank);
                    (dense * DENSE_WEIGHT + rrf * RRF_WEIGHT, HYBRID_WEIGHT_TOTAL)
                }
            };
            ScoredCandidate {
                original_rank: candidate.original_rank,
                score: (lexical + extra) / weight_total,
                hit: candidate.hit,
                matched_terms,
                all_terms: candidate.unique_terms,
                adjacency,
                span,
                code,
                facet_rank: candidate.facet_rank,
                facet_primary_mask: candidate.facet_primary_mask,
                admitted_by_evidence_gate: false,
                admitted_by_semantic_gate: false,
                adds_new_query_evidence: false,
            }
        })
        .collect()
}

fn normalized_rrf(sparse_rank: Option<usize>, dense_rank: Option<usize>) -> f64 {
    let score = sparse_rank
        .into_iter()
        .chain(dense_rank)
        .map(|rank| 1.0 / (RRF_K + (rank + 1) as f64))
        .sum::<f64>();
    score / (2.0 / (RRF_K + 1.0))
}

fn into_hits(candidates: Vec<ScoredCandidate>) -> Vec<MemoryHit> {
    candidates
        .into_iter()
        .map(|candidate| MemoryHit {
            score: candidate.score,
            ..candidate.hit
        })
        .collect()
}

fn jaccard_similarity(left: &BTreeSet<String>, right: &BTreeSet<String>) -> f64 {
    if left.is_empty() && right.is_empty() {
        return 0.0;
    }
    let intersection = left.intersection(right).count();
    let union = left.len() + right.len() - intersection;
    intersection as f64 / union as f64
}

fn inverse_document_weights(
    query_terms: &BTreeSet<String>,
    candidates: &[Candidate],
) -> BTreeMap<String, f64> {
    query_terms
        .iter()
        .map(|term| {
            let document_frequency = candidates
                .iter()
                .filter(|candidate| candidate.unique_terms.contains(term))
                .count();
            let inverse_document_frequency =
                ((candidates.len() + 1) as f64 / (document_frequency + 1) as f64).ln() + 1.0;
            let length_boost = 1.0 + term.chars().count().min(12) as f64 / 40.0;
            (term.clone(), inverse_document_frequency * length_boost)
        })
        .collect()
}

fn weighted_coverage(
    query_terms: &BTreeSet<String>,
    record_terms: &BTreeSet<String>,
    weights: &BTreeMap<String, f64>,
) -> f64 {
    let total = query_terms
        .iter()
        .map(|term| weights.get(term).copied().unwrap_or(1.0))
        .sum::<f64>();
    if total == 0.0 {
        return 0.0;
    }
    query_terms
        .iter()
        .filter(|term| record_terms.contains(*term))
        .map(|term| weights.get(term).copied().unwrap_or(1.0))
        .sum::<f64>()
        / total
}

fn adjacent_pair_coverage(query: &[String], record: &[String]) -> f64 {
    if query.len() < 2 {
        return f64::from(query.first().is_some_and(|term| record.contains(term)));
    }
    let matched = query
        .windows(2)
        .filter(|pair| record.windows(2).any(|candidate| candidate == *pair))
        .count();
    matched as f64 / (query.len() - 1) as f64
}

fn contiguous_span_coverage(query: &[String], record: &[String]) -> f64 {
    if query.is_empty() {
        return 0.0;
    }
    let mut previous = vec![0_usize; record.len() + 1];
    let mut longest = 0;
    for query_term in query {
        let mut current = vec![0_usize; record.len() + 1];
        for (record_index, record_term) in record.iter().enumerate() {
            if query_term == record_term {
                current[record_index + 1] = previous[record_index] + 1;
                longest = longest.max(current[record_index + 1]);
            }
        }
        previous = current;
    }
    longest as f64 / query.len() as f64
}

fn set_coverage(query: &BTreeSet<String>, record: &BTreeSet<String>) -> f64 {
    if query.is_empty() {
        return 0.0;
    }
    query.iter().filter(|atom| record.contains(*atom)).count() as f64 / query.len() as f64
}

fn terms(text: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut buffered = String::new();
    for character in text.chars() {
        if is_cjk(character) {
            flush_term(&mut buffered, &mut terms);
            terms.push(character.to_string());
        } else if character.is_alphanumeric() || character == '_' {
            buffered.extend(character.to_lowercase());
        } else {
            flush_term(&mut buffered, &mut terms);
        }
    }
    flush_term(&mut buffered, &mut terms);
    terms
}

fn flush_term(buffered: &mut String, terms: &mut Vec<String>) {
    if !buffered.is_empty() {
        terms.push(std::mem::take(buffered));
    }
}

fn code_atoms(text: &str) -> BTreeSet<String> {
    text.split_whitespace()
        .map(|atom| {
            atom.trim_matches(|character: char| {
                matches!(
                    character,
                    ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | '`' | '?' | '!'
                )
            })
            .to_lowercase()
        })
        .filter(|atom| {
            atom.chars().any(|character| character.is_alphanumeric())
                && atom
                    .chars()
                    .any(|character| matches!(character, '_' | '-' | '/' | ':' | '.' | '='))
        })
        .collect()
}

fn is_cjk(character: char) -> bool {
    matches!(character, '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}')
}

fn is_substantive_term(term: &str) -> bool {
    term.chars().any(is_cjk)
        || term.chars().count() >= MIN_SUBSTANTIVE_TERM_CHARS
        || term.chars().any(|character| character.is_ascii_digit())
}

fn adds_distinct_query_evidence(candidate: &ScoredCandidate, covered: &BTreeSet<String>) -> bool {
    adds_new_substantive_query_evidence(candidate, covered)
        || candidate
            .matched_terms
            .iter()
            .filter(|term| is_substantive_term(term))
            .take(MIN_REPEATED_EVIDENCE_TERMS)
            .count()
            == MIN_REPEATED_EVIDENCE_TERMS
}

fn adds_new_substantive_query_evidence(
    candidate: &ScoredCandidate,
    covered: &BTreeSet<String>,
) -> bool {
    candidate
        .matched_terms
        .difference(covered)
        .any(|term| is_substantive_term(term))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MemoryEvidence, MemoryKind, MemoryOrigin, MemoryRecord, MemoryScope};

    fn hit(id: &str, kind: MemoryKind, text: &str) -> MemoryHit {
        MemoryHit {
            record: MemoryRecord {
                id: id.to_string(),
                scope: MemoryScope::User,
                kind,
                text: text.to_string(),
                origin: MemoryOrigin {
                    session_id: "session".to_string(),
                    entry_id: None,
                    tool_call_id: None,
                },
                evidence: MemoryEvidence {
                    note: "evidence".to_string(),
                },
                recorded_at_ms: 1,
                supersedes: None,
            },
            score: 0.0,
        }
    }

    #[test]
    fn exact_command_and_order_can_beat_a_higher_sparse_candidate() {
        let hits = vec![
            hit(
                "broad",
                MemoryKind::Summary,
                "Atlas release dashboard mentions workspace approval, cargo artifacts, and test status.",
            ),
            hit(
                "exact",
                MemoryKind::Instruction,
                "Atlas full test command is cargo test --workspace.",
            ),
        ];
        let ranked = rerank("Atlas full test command cargo workspace", hits, 2);
        assert_eq!(ranked[0].record.id, "exact");
    }

    #[test]
    fn cjk_terms_are_tokenized_without_an_intent_vocabulary() {
        assert_eq!(
            terms("当前回答 style"),
            ["当", "前", "回", "答", "style"].map(String::from)
        );
    }

    #[test]
    fn record_kind_does_not_change_lexical_ranking() {
        let hits = vec![
            hit(
                "first",
                MemoryKind::Fact,
                "Prefer a concise response style.",
            ),
            hit(
                "second",
                MemoryKind::Decision,
                "Prefer a concise response style.",
            ),
            hit(
                "third",
                MemoryKind::Preference,
                "Prefer a concise response style.",
            ),
        ];
        let ranked = rerank("prefer concise response style", hits, 3);
        assert_eq!(
            ranked
                .iter()
                .map(|hit| hit.record.id.as_str())
                .collect::<Vec<_>>(),
            ["first", "second", "third"]
        );
    }

    #[test]
    fn complementary_evidence_is_selected_over_redundant_summaries() {
        let mut hits = vec![hit(
            "workflow",
            MemoryKind::Instruction,
            "Atlas release workflow runs the canonical test command, builds a signed artifact, stages it, and waits for production approval.",
        )];
        hits.extend((0..7).map(|index| {
            hit(
                &format!("summary-{index}"),
                MemoryKind::Summary,
                "Atlas release dashboard tracks signed artifact staging production approval status.",
            )
        }));
        hits.push(hit(
            "test-command",
            MemoryKind::Instruction,
            "Atlas full test command is cargo test --workspace.",
        ));
        let ranked = rerank(
            "Atlas release workflow cargo test workspace signed artifact staging production approval",
            hits,
            5,
        );
        assert!(ranked.iter().any(|hit| hit.record.id == "test-command"));
    }

    #[test]
    fn candidate_window_is_bounded() {
        assert_eq!(candidate_limit(1), 32);
        assert_eq!(candidate_limit(8), 64);
        assert_eq!(candidate_limit(100), 100);
    }

    #[test]
    fn dense_only_noise_is_cut_off_behind_a_lexical_core() {
        let sparse = vec![hit(
            "lexical",
            MemoryKind::Instruction,
            "Atlas full test command is cargo test --workspace.",
        )];
        let mut noise = hit(
            "dense-only",
            MemoryKind::Fact,
            "The deployment hue is ultramarine.",
        );
        noise.score = -0.01;
        let dense = vec![noise];

        let fused = fuse_sparse_dense("Atlas full test command cargo workspace", sparse, dense, 3);

        assert_eq!(
            fused
                .iter()
                .map(|hit| hit.record.id.as_str())
                .collect::<Vec<_>>(),
            ["lexical"]
        );
    }
}
