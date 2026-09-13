use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{EvalExecutionOutcome, EvalRun};

pub const EVAL_COMPARISON_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalComparisonDefinition {
    pub eval_set: String,
    pub case_ids: Vec<String>,
    pub baseline: String,
    pub candidates: Vec<String>,
    pub repetitions: u32,
}

impl EvalComparisonDefinition {
    pub fn new(
        eval_set: impl Into<String>,
        case_ids: impl IntoIterator<Item = impl Into<String>>,
        baseline: impl Into<String>,
        candidates: impl IntoIterator<Item = impl Into<String>>,
        repetitions: u32,
    ) -> Self {
        Self {
            eval_set: eval_set.into(),
            case_ids: case_ids.into_iter().map(Into::into).collect(),
            baseline: baseline.into(),
            candidates: candidates.into_iter().map(Into::into).collect(),
            repetitions,
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.eval_set.trim().is_empty() {
            return Err("eval set must not be empty".to_string());
        }
        if self.case_ids.is_empty() {
            return Err(format!("eval set {} has no cases", self.eval_set));
        }
        if self.baseline.trim().is_empty() {
            return Err(format!("eval set {} has no baseline", self.eval_set));
        }
        if self.candidates.is_empty() {
            return Err(format!("eval set {} has no candidates", self.eval_set));
        }
        if self.repetitions == 0 {
            return Err(format!("eval set {} has no repetitions", self.eval_set));
        }
        let mut names = BTreeSet::from([self.baseline.as_str()]);
        if self
            .candidates
            .iter()
            .any(|candidate| candidate.trim().is_empty() || !names.insert(candidate))
        {
            return Err(format!(
                "eval set {} has empty or duplicate harness names",
                self.eval_set
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairedMetricSummary {
    pub total_pairs: u64,
    pub eligible_pairs: u64,
    pub baseline_mean: Option<f64>,
    pub candidate_mean: Option<f64>,
    pub mean_delta: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CorrectnessLiftSummary {
    pub total_pairs: u64,
    pub eligible_pairs: u64,
    pub baseline_pass_rate: Option<f64>,
    pub candidate_pass_rate: Option<f64>,
    pub lift: Option<f64>,
    pub baseline_wins: u64,
    pub candidate_wins: u64,
    pub ties: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessPairComparison {
    pub baseline: String,
    pub candidate: String,
    pub correctness: CorrectnessLiftSummary,
    pub total_tokens: PairedMetricSummary,
    pub total_ms: PairedMetricSummary,
    pub estimated_cost_usd: PairedMetricSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ComparisonDiagnosticReason {
    MissingObservation,
    DuplicateObservation,
    HarnessError,
    MissingScore,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessComparisonDiagnostic {
    pub eval_set: String,
    pub case_id: String,
    pub repetition: u32,
    pub harness: String,
    pub reason: ComparisonDiagnosticReason,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessEvalSetReport {
    pub eval_set: String,
    pub comparisons: Vec<HarnessPairComparison>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessComparisonReport {
    pub schema_version: u32,
    pub eval_sets: Vec<HarnessEvalSetReport>,
    pub diagnostics: Vec<HarnessComparisonDiagnostic>,
}

#[derive(Clone, Copy)]
struct ScoredRun<'a> {
    run: &'a EvalRun,
    score: f64,
}

pub fn summarize_comparisons(
    definitions: &[EvalComparisonDefinition],
    runs: &[EvalRun],
) -> Result<HarnessComparisonReport, String> {
    let mut eval_sets = Vec::new();
    let mut diagnostics = Vec::new();
    let mut definitions = definitions.iter().collect::<Vec<_>>();
    definitions.sort_by(|left, right| left.eval_set.cmp(&right.eval_set));

    for definition in definitions {
        definition.validate()?;
        let mut comparisons = Vec::new();
        for candidate in &definition.candidates {
            let mut pairs = Vec::new();
            let mut total_pairs = 0_u64;
            for case_id in &definition.case_ids {
                for repetition in 1..=definition.repetitions {
                    total_pairs = total_pairs.saturating_add(1);
                    let baseline = matching_runs(runs, case_id, repetition, &definition.baseline);
                    let candidate_runs = matching_runs(runs, case_id, repetition, candidate);
                    collect_diagnostic(
                        &mut diagnostics,
                        definition,
                        case_id,
                        repetition,
                        &definition.baseline,
                        &baseline,
                    );
                    collect_diagnostic(
                        &mut diagnostics,
                        definition,
                        case_id,
                        repetition,
                        candidate,
                        &candidate_runs,
                    );
                    if baseline.len() == 1
                        && candidate_runs.len() == 1
                        && let (Some(baseline), Some(candidate_run)) =
                            (score_run(baseline[0]), score_run(candidate_runs[0]))
                    {
                        pairs.push((baseline, candidate_run));
                    }
                }
            }
            comparisons.push(compare_pair(
                &definition.baseline,
                candidate,
                &pairs,
                total_pairs,
            ));
        }
        eval_sets.push(HarnessEvalSetReport {
            eval_set: definition.eval_set.clone(),
            comparisons,
        });
    }

    diagnostics.sort_by(|left, right| {
        left.eval_set
            .cmp(&right.eval_set)
            .then_with(|| left.case_id.cmp(&right.case_id))
            .then_with(|| left.repetition.cmp(&right.repetition))
            .then_with(|| left.harness.cmp(&right.harness))
    });
    diagnostics.dedup();
    Ok(HarnessComparisonReport {
        schema_version: EVAL_COMPARISON_SCHEMA_VERSION,
        eval_sets,
        diagnostics,
    })
}

fn matching_runs<'a>(
    runs: &'a [EvalRun],
    case_id: &str,
    repetition: u32,
    variant: &str,
) -> Vec<&'a EvalRun> {
    runs.iter()
        .filter(|run| {
            run.case_id == case_id && run.repetition == repetition && run.variant == variant
        })
        .collect()
}

fn collect_diagnostic(
    diagnostics: &mut Vec<HarnessComparisonDiagnostic>,
    definition: &EvalComparisonDefinition,
    case_id: &str,
    repetition: u32,
    harness: &str,
    runs: &[&EvalRun],
) {
    let reason = if runs.is_empty() {
        Some(ComparisonDiagnosticReason::MissingObservation)
    } else if runs.len() > 1 {
        Some(ComparisonDiagnosticReason::DuplicateObservation)
    } else if runs[0].execution_outcome != EvalExecutionOutcome::Completed {
        Some(ComparisonDiagnosticReason::HarnessError)
    } else if score_run(runs[0]).is_none() {
        Some(ComparisonDiagnosticReason::MissingScore)
    } else {
        None
    };
    if let Some(reason) = reason {
        diagnostics.push(HarnessComparisonDiagnostic {
            eval_set: definition.eval_set.clone(),
            case_id: case_id.to_string(),
            repetition,
            harness: harness.to_string(),
            reason,
        });
    }
}

fn score_run(run: &EvalRun) -> Option<ScoredRun<'_>> {
    if run.execution_outcome != EvalExecutionOutcome::Completed || run.grades.is_empty() {
        return None;
    }
    let sum = run.grades.iter().try_fold(0.0, |sum, grade| {
        grade.score.is_finite().then_some(sum + grade.score)
    })?;
    Some(ScoredRun {
        run,
        score: sum / run.grades.len() as f64,
    })
}

fn compare_pair(
    baseline_name: &str,
    candidate_name: &str,
    pairs: &[(ScoredRun<'_>, ScoredRun<'_>)],
    total_pairs: u64,
) -> HarnessPairComparison {
    let mut baseline_passes = 0_u64;
    let mut candidate_passes = 0_u64;
    let mut baseline_wins = 0_u64;
    let mut candidate_wins = 0_u64;
    let mut ties = 0_u64;
    for (baseline, candidate) in pairs {
        let baseline_passed = baseline.score >= 1.0;
        let candidate_passed = candidate.score >= 1.0;
        baseline_passes = baseline_passes.saturating_add(u64::from(baseline_passed));
        candidate_passes = candidate_passes.saturating_add(u64::from(candidate_passed));
        match (baseline_passed, candidate_passed) {
            (true, false) => baseline_wins = baseline_wins.saturating_add(1),
            (false, true) => candidate_wins = candidate_wins.saturating_add(1),
            _ => ties = ties.saturating_add(1),
        }
    }
    let eligible_pairs = u64::try_from(pairs.len()).unwrap_or(u64::MAX);
    let baseline_pass_rate = mean_ratio(baseline_passes, eligible_pairs);
    let candidate_pass_rate = mean_ratio(candidate_passes, eligible_pairs);
    HarnessPairComparison {
        baseline: baseline_name.to_string(),
        candidate: candidate_name.to_string(),
        correctness: CorrectnessLiftSummary {
            total_pairs,
            eligible_pairs,
            baseline_pass_rate,
            candidate_pass_rate,
            lift: option_difference(candidate_pass_rate, baseline_pass_rate),
            baseline_wins,
            candidate_wins,
            ties,
        },
        total_tokens: summarize_metric(pairs, total_pairs, |run| {
            Some(run.observation.usage.total_tokens as f64)
        }),
        total_ms: summarize_metric(pairs, total_pairs, |run| Some(run.duration_ms as f64)),
        estimated_cost_usd: summarize_metric(pairs, total_pairs, |run| {
            run.observation.usage.estimated_cost_usd
        }),
    }
}

fn summarize_metric(
    pairs: &[(ScoredRun<'_>, ScoredRun<'_>)],
    total_pairs: u64,
    select: impl Fn(&EvalRun) -> Option<f64>,
) -> PairedMetricSummary {
    let values = pairs
        .iter()
        .filter_map(|(baseline, candidate)| {
            let baseline = select(baseline.run)?;
            let candidate = select(candidate.run)?;
            (baseline.is_finite() && candidate.is_finite()).then_some((baseline, candidate))
        })
        .collect::<Vec<_>>();
    let baseline_mean = mean(values.iter().map(|(baseline, _)| *baseline));
    let candidate_mean = mean(values.iter().map(|(_, candidate)| *candidate));
    PairedMetricSummary {
        total_pairs,
        eligible_pairs: u64::try_from(values.len()).unwrap_or(u64::MAX),
        baseline_mean,
        candidate_mean,
        mean_delta: option_difference(candidate_mean, baseline_mean),
    }
}

fn mean(values: impl Iterator<Item = f64>) -> Option<f64> {
    let mut sum = 0.0;
    let mut count = 0_u64;
    for value in values {
        sum += value;
        count = count.saturating_add(1);
    }
    (count > 0).then_some(sum / count as f64)
}

fn mean_ratio(value: u64, count: u64) -> Option<f64> {
    (count > 0).then_some(value as f64 / count as f64)
}

fn option_difference(left: Option<f64>, right: Option<f64>) -> Option<f64> {
    Some(left? - right?)
}

pub fn format_comparison_report(report: &HarnessComparisonReport) -> String {
    let mut lines = vec!["Eval Comparisons".to_string()];
    for eval_set in &report.eval_sets {
        lines.push(format!("  {}", eval_set.eval_set));
        for comparison in &eval_set.comparisons {
            let correctness = &comparison.correctness;
            lines.push(format!("     Baseline  {}", comparison.baseline));
            lines.push(format!(
                "    Candidate  {} ({}/{} pairs)",
                comparison.candidate, correctness.eligible_pairs, correctness.total_pairs
            ));
            lines.push(format!(
                "    Pass rate  {}",
                match (
                    correctness.lift,
                    correctness.candidate_pass_rate,
                    correctness.baseline_pass_rate,
                ) {
                    (Some(lift), Some(candidate), Some(baseline)) => format!(
                        "{:+.1} pp (candidate {:.1}%, baseline {:.1}%)",
                        lift * 100.0,
                        candidate * 100.0,
                        baseline * 100.0
                    ),
                    _ => "unavailable".to_string(),
                }
            ));
            lines.push(format_metric("Tokens", &comparison.total_tokens, "", 1));
            lines.push(format_metric("Latency", &comparison.total_ms, "ms", 1));
            lines.push(format_cost(&comparison.estimated_cost_usd));
        }
    }
    if !report.diagnostics.is_empty() {
        lines.push(format!(
            "  Incomplete observations: {}",
            report.diagnostics.len()
        ));
    }
    lines.join("\n")
}

fn format_metric(label: &str, metric: &PairedMetricSummary, suffix: &str, digits: usize) -> String {
    match (
        metric.mean_delta,
        metric.candidate_mean,
        metric.baseline_mean,
    ) {
        (Some(delta), Some(candidate), Some(baseline)) => format!(
            "    {:>9}  {:+.*}{} (candidate {:.*}{}, baseline {:.*}{})",
            label, digits, delta, suffix, digits, candidate, suffix, digits, baseline, suffix
        ),
        _ => format!("    {:>9}  unavailable", label),
    }
}

fn format_cost(metric: &PairedMetricSummary) -> String {
    match (
        metric.mean_delta,
        metric.candidate_mean,
        metric.baseline_mean,
    ) {
        (Some(delta), Some(candidate), Some(baseline)) => format!(
            "    {:>9}  {}${:.4} (candidate ${:.4}, baseline ${:.4})",
            "Est. cost",
            if delta >= 0.0 { "+" } else { "-" },
            delta.abs(),
            candidate,
            baseline
        ),
        _ => "    Est. cost  unavailable".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EvalGrade, EvalObservation, EvalUsage};

    fn run(case: &str, variant: &str, repetition: u32, score: f64, tokens: u64) -> EvalRun {
        EvalRun {
            schema_version: 1,
            run_id: format!("{case}-{variant}-{repetition}"),
            case_id: case.to_string(),
            variant: variant.to_string(),
            provider: "fixture".to_string(),
            model: "fixture".to_string(),
            repetition,
            started_at_ms: 0,
            duration_ms: tokens * 10,
            execution_outcome: EvalExecutionOutcome::Completed,
            passed: score >= 1.0,
            observation: EvalObservation {
                system_prompt: None,
                final_response: String::new(),
                transcript: Vec::new(),
                workspace_changes: Vec::new(),
                usage: EvalUsage {
                    total_tokens: tokens,
                    estimated_cost_usd: Some(tokens as f64 / 1000.0),
                    ..EvalUsage::default()
                },
                errors: Vec::new(),
            },
            grades: vec![EvalGrade {
                grader: "fixture".to_string(),
                score,
                passed: score >= 1.0,
                required: true,
                rationale: String::new(),
            }],
            artifacts: Vec::new(),
        }
    }

    #[test]
    fn computes_paired_lift_and_keeps_missing_runs_out_of_metrics() {
        let definition = EvalComparisonDefinition::new(
            "authoring",
            ["case-a", "case-b"],
            "without-docs",
            ["with-docs"],
            1,
        );
        let runs = vec![
            run("case-a", "without-docs", 1, 0.0, 100),
            run("case-a", "with-docs", 1, 1.0, 80),
            run("case-b", "with-docs", 1, 1.0, 70),
        ];
        let report = summarize_comparisons(&[definition], &runs).unwrap();
        let comparison = &report.eval_sets[0].comparisons[0];
        assert_eq!(comparison.correctness.total_pairs, 2);
        assert_eq!(comparison.correctness.eligible_pairs, 1);
        assert_eq!(comparison.correctness.lift, Some(1.0));
        assert_eq!(comparison.total_tokens.mean_delta, Some(-20.0));
        assert_eq!(report.diagnostics.len(), 1);
        assert_eq!(
            report.diagnostics[0].reason,
            ComparisonDiagnosticReason::MissingObservation
        );
        let formatted = format_comparison_report(&report);
        assert!(formatted.contains("+100.0 pp"));
        assert!(formatted.contains("-20.0"));
    }

    #[test]
    fn does_not_turn_harness_errors_or_missing_scores_into_failures() {
        let definition =
            EvalComparisonDefinition::new("authoring", ["case-a"], "baseline", ["candidate"], 1);
        let mut baseline = run("case-a", "baseline", 1, 0.0, 100);
        baseline.execution_outcome = EvalExecutionOutcome::Errored;
        let mut candidate = run("case-a", "candidate", 1, 1.0, 80);
        candidate.grades.clear();
        let report = summarize_comparisons(&[definition], &[baseline, candidate]).unwrap();
        assert_eq!(
            report.eval_sets[0].comparisons[0]
                .correctness
                .eligible_pairs,
            0
        );
        assert_eq!(report.diagnostics.len(), 2);
        assert!(
            report.diagnostics.iter().any(|diagnostic| {
                diagnostic.reason == ComparisonDiagnosticReason::HarnessError
            })
        );
        assert!(
            report.diagnostics.iter().any(|diagnostic| {
                diagnostic.reason == ComparisonDiagnosticReason::MissingScore
            })
        );
    }

    #[test]
    fn rejects_duplicate_harness_names() {
        let definition =
            EvalComparisonDefinition::new("authoring", ["case-a"], "same", ["same"], 1);
        assert!(summarize_comparisons(&[definition], &[]).is_err());
    }
}
