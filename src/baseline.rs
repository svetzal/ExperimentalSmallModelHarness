//! The preserved 30-cell cross-model demo matrix, as machine-readable
//! evidence.
//!
//! `GENERALIZATION_PLAN.md` (Slice 0, "Freeze The Baseline") requires the
//! completed demo matrix to be an explicit architectural baseline that later
//! experiments can read without parsing narrative notes or invoking
//! matrix-specific classification code. [`MatrixBaseline`] is that
//! machine-readable evidence; it is loaded from `baseline/matrix_baseline.json`
//! (committed alongside the crate) and its documented boundaries are typed
//! against the same canonical [`crate::trace_analysis::RunOutcome`] the
//! analyzer produces, so a fixture's classification and a baseline boundary's
//! classification are the same enum, not independently-maintained strings.

use crate::trace_analysis::{
    EnvironmentValidity, HardStopReason, HarnessCompletion, IndependentValidation, RunOutcome,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One documented outcome boundary observed during the demo matrix, tied to
/// the canonical [`RunOutcome`] classification it represents.
#[derive(Debug, Clone, Deserialize)]
pub struct MatrixBoundary {
    pub id: String,
    pub classification: RunOutcome,
    pub model: Option<String>,
    pub task: Option<String>,
    pub detail: String,
}

/// One preserved matrix cell (one model x one task), typed against the same
/// canonical facts [`crate::trace_analysis::TraceAnalysis`] exposes for a
/// single trace: harness completion, independent validation, and validation
/// environment validity are three separate typed facts, not one collapsed
/// pass/fail.
///
/// Sourced verbatim from `Demos/Matrix/results.tsv` (read-only parity
/// oracle); see `GENERALIZATION_PLAN.md` Slice 1.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MatrixCell {
    pub model: String,
    pub model_slug: String,
    pub task: String,
    pub harness_completion: HarnessCompletion,
    pub hard_stop: Option<HardStopReason>,
    pub independent_validation: IndependentValidation,
    pub environment_validity: EnvironmentValidity,
}

/// The preserved 30-cell cross-model demo matrix.
#[derive(Debug, Clone, Deserialize)]
pub struct MatrixBaseline {
    pub schema_version: u32,
    pub description: String,
    pub evidence_class: String,
    pub replicates_per_cell: u32,
    pub planned_cells: u32,
    pub completed_cells: u32,
    pub independently_validated_cells: u32,
    pub tasks: Vec<String>,
    pub model_configurations_screened: u32,
    pub configurations_passing_every_task: u32,
    pub configurations_failing_at_action_boundary: u32,
    pub documented_boundaries: Vec<MatrixBoundary>,
    pub cells: Vec<MatrixCell>,
}

/// Canonical counts derived from [`MatrixCell`] records — the replacement
/// for `Demos/Matrix/summarize.rb`'s bespoke classification. Every count
/// here is a pure tally over the typed facts already on [`MatrixCell`]; no
/// matrix-specific classification logic is involved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MatrixSummary {
    pub completed_cells: usize,
    pub independently_validated_passes: usize,
    pub independent_validation_failures: usize,
    pub independent_validation_unknown: usize,
    pub hard_stops: usize,
    pub hard_stop_counts_by_reason: BTreeMap<String, usize>,
    pub environment_corrections: usize,
    pub passes_by_model: BTreeMap<String, usize>,
    pub passes_by_task: BTreeMap<String, usize>,
    /// `"{model_slug}/{task}"` identities of cells that did not pass
    /// independent validation, sorted for deterministic output.
    pub failing_cells: Vec<String>,
}

/// Summarize preserved matrix cells into canonical counts.
///
/// A cell is "completed" unconditionally (this baseline only preserves
/// completed cells) and counts as an independently-validated pass only when
/// [`MatrixCell::harness_completion`] reached
/// [`HarnessCompletion::Finished`] *and*
/// [`MatrixCell::independent_validation`] is
/// [`IndependentValidation::Passed`] — harness completion and independent
/// validation are checked separately, never conflated.
pub fn summarize_matrix(cells: &[MatrixCell]) -> MatrixSummary {
    let mut independently_validated_passes = 0;
    let mut independent_validation_failures = 0;
    let mut independent_validation_unknown = 0;
    let mut hard_stops = 0;
    let mut hard_stop_counts_by_reason = BTreeMap::new();
    let mut environment_corrections = 0;
    let mut passes_by_model = BTreeMap::new();
    let mut passes_by_task = BTreeMap::new();
    let mut failing_cells = Vec::new();

    for cell in cells {
        let passed = cell.independent_validation == IndependentValidation::Passed;
        match cell.independent_validation {
            IndependentValidation::Passed => independently_validated_passes += 1,
            IndependentValidation::Failed => independent_validation_failures += 1,
            IndependentValidation::Unknown => independent_validation_unknown += 1,
        }
        if passed {
            *passes_by_model.entry(cell.model_slug.clone()).or_insert(0) += 1;
            *passes_by_task.entry(cell.task.clone()).or_insert(0) += 1;
        } else {
            failing_cells.push(format!("{}/{}", cell.model_slug, cell.task));
        }
        if cell.harness_completion == HarnessCompletion::HardStopped {
            hard_stops += 1;
        }
        if let Some(reason) = cell.hard_stop {
            *hard_stop_counts_by_reason
                .entry(reason.as_str().to_string())
                .or_insert(0) += 1;
        }
        if cell.environment_validity == EnvironmentValidity::Corrected {
            environment_corrections += 1;
        }
    }
    failing_cells.sort();

    MatrixSummary {
        completed_cells: cells.len(),
        independently_validated_passes,
        independent_validation_failures,
        independent_validation_unknown,
        hard_stops,
        hard_stop_counts_by_reason,
        environment_corrections,
        passes_by_model,
        passes_by_task,
        failing_cells,
    }
}

const MATRIX_BASELINE_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/baseline/matrix_baseline.json"
));

/// Load the committed matrix baseline.
///
/// # Panics
///
/// Panics if the committed `baseline/matrix_baseline.json` is malformed or
/// disagrees with the [`MatrixBoundary`]/[`MatrixBaseline`] shape. That file
/// is repository-owned evidence, not user input, so a parse failure here
/// indicates a broken build, not a runtime condition to recover from.
pub fn load_matrix_baseline() -> MatrixBaseline {
    serde_json::from_str(MATRIX_BASELINE_JSON)
        .expect("baseline/matrix_baseline.json must parse as MatrixBaseline")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_the_completed_30_cell_matrix_invariants() {
        let baseline = load_matrix_baseline();

        assert_eq!(baseline.planned_cells, 30);
        assert_eq!(baseline.completed_cells, 30);
        assert!(baseline.independently_validated_cells <= baseline.completed_cells);
        assert_eq!(baseline.tasks.len(), 5);
        assert_eq!(
            baseline.configurations_passing_every_task
                + baseline.configurations_failing_at_action_boundary,
            baseline.model_configurations_screened
        );

        assert_eq!(baseline.cells.len(), 30);
        let derived_passes = baseline
            .cells
            .iter()
            .filter(|cell| cell.independent_validation == IndependentValidation::Passed)
            .count();
        assert_eq!(
            derived_passes as u32,
            baseline.independently_validated_cells
        );
    }

    #[test]
    fn documented_boundaries_use_the_canonical_run_outcome_enum() {
        let baseline = load_matrix_baseline();
        let classifications: std::collections::BTreeMap<_, _> = baseline
            .documented_boundaries
            .iter()
            .map(|boundary| (boundary.id.as_str(), boundary.classification))
            .collect();

        assert_eq!(
            classifications.get("hidden_only_all_tasks"),
            Some(&RunOutcome::HiddenOnlyNoActionStop)
        );
        assert_eq!(
            classifications.get("qwen_nvfp4_go_action_boundary"),
            Some(&RunOutcome::ActionBoundaryStop)
        );
        assert_eq!(
            classifications.get("ruby_environment_invalid"),
            Some(&RunOutcome::EnvironmentInvalidValidation)
        );
    }

    #[test]
    fn matrix_summary_matches_the_preserved_oracle() {
        let baseline = load_matrix_baseline();
        let summary = summarize_matrix(&baseline.cells);

        // Demos/Matrix/results.tsv / results.md: 30 completed cells, 24
        // independently-validated passes, 6 failures, 6 hard-stops.
        assert_eq!(summary.completed_cells, 30);
        assert_eq!(summary.independently_validated_passes, 24);
        assert_eq!(summary.independent_validation_failures, 6);
        assert_eq!(summary.independent_validation_unknown, 0);
        assert_eq!(summary.hard_stops, 6);

        let expected_reasons: BTreeMap<String, usize> = [
            ("action_boundary".to_string(), 1),
            ("hidden_only_no_action".to_string(), 5),
        ]
        .into_iter()
        .collect();
        assert_eq!(summary.hard_stop_counts_by_reason, expected_reasons);

        // Every ruby-ini-parser cell required an explicit environment
        // correction, independent of the model's pass/fail.
        assert_eq!(summary.environment_corrections, 6);

        let expected_failing_cells = vec![
            "gemma4-latest/go-shortest-path".to_string(),
            "gemma4-latest/javascript-topological-sort".to_string(),
            "gemma4-latest/python-expression-parser".to_string(),
            "gemma4-latest/ruby-ini-parser".to_string(),
            "gemma4-latest/rust-binary-search-tree".to_string(),
            "qwen36-35b-nvfp4/go-shortest-path".to_string(),
        ];
        assert_eq!(summary.failing_cells, expected_failing_cells);

        let expected_passes_by_model: BTreeMap<String, usize> = [
            ("gemma4-31b-mlx".to_string(), 5),
            ("qwen36-35b-nvfp4".to_string(), 4),
            ("qwen36-27b-bf16".to_string(), 5),
            ("qwen36-27b-mxfp8".to_string(), 5),
            ("qwen35-122b".to_string(), 5),
        ]
        .into_iter()
        .collect();
        assert_eq!(summary.passes_by_model, expected_passes_by_model);

        let expected_passes_by_task: BTreeMap<String, usize> = [
            ("rust-binary-search-tree".to_string(), 5),
            ("python-expression-parser".to_string(), 5),
            ("go-shortest-path".to_string(), 4),
            ("javascript-topological-sort".to_string(), 5),
            ("ruby-ini-parser".to_string(), 5),
        ]
        .into_iter()
        .collect();
        assert_eq!(summary.passes_by_task, expected_passes_by_task);
    }

    #[test]
    fn matrix_summary_is_deterministic_across_invocations() {
        let baseline = load_matrix_baseline();

        let first = summarize_matrix(&baseline.cells);
        let second = summarize_matrix(&baseline.cells);

        assert_eq!(
            serde_json::to_string_pretty(&first).unwrap(),
            serde_json::to_string_pretty(&second).unwrap()
        );
    }

    #[test]
    fn preserved_matrix_covers_exactly_the_six_by_five_cell_identity() {
        let baseline = load_matrix_baseline();

        let identities: std::collections::BTreeSet<(String, String)> = baseline
            .cells
            .iter()
            .map(|cell| (cell.model_slug.clone(), cell.task.clone()))
            .collect();

        // 30 unique (model_slug, task) identities: 6 models x 5 tasks, no
        // duplicates and no gaps.
        assert_eq!(identities.len(), 30);
        assert_eq!(baseline.cells.len(), 30);

        let models: std::collections::BTreeSet<_> =
            identities.iter().map(|(model, _)| model.clone()).collect();
        let tasks: std::collections::BTreeSet<_> =
            identities.iter().map(|(_, task)| task.clone()).collect();
        assert_eq!(models.len(), 6);
        assert_eq!(tasks.len(), 5);
    }
}
