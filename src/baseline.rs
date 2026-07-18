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

use crate::trace_analysis::RunOutcome;
use serde::Deserialize;

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
}
