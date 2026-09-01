//! Worker-visible coverage tracking for accepted acceptance-planning output.
//!
//! The ledger is deliberately weaker than declared probes: it records that the
//! worker addressed public requirements and cited evidence after the latest
//! mutation, but it does not assert that the evidence is semantically correct.

use crate::acceptance_interactions::AcceptanceInteractions;
use crate::acceptance_plan::AcceptancePlan;
use anyhow::{Result, bail};
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};

pub const ACCEPTANCE_LEDGER_SCHEMA_VERSION: &str = "acceptance_ledger.v1";
pub const MAX_ACCEPTANCE_EVIDENCE_CHARS: usize = 8_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceptanceLedgerEntryKind {
    Requirement,
    Interaction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcceptanceLedgerEntry {
    pub id: String,
    pub kind: AcceptanceLedgerEntryKind,
    pub statement: String,
    pub suggested_evidence: String,
    pub source_excerpt: Option<String>,
    pub linked_item_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcceptanceLedgerSpec {
    pub schema_version: String,
    pub entries: Vec<AcceptanceLedgerEntry>,
}

impl AcceptanceLedgerSpec {
    pub fn from_plans(
        plan: &AcceptancePlan,
        interactions: &AcceptanceInteractions,
    ) -> Result<Self> {
        let mut entries = Vec::with_capacity(plan.items.len() + interactions.scenarios.len());
        let mut ids = HashSet::new();
        for item in &plan.items {
            if !ids.insert(item.id.clone()) {
                bail!("duplicate acceptance ledger id {:?}", item.id);
            }
            entries.push(AcceptanceLedgerEntry {
                id: item.id.clone(),
                kind: AcceptanceLedgerEntryKind::Requirement,
                statement: item.requirement.clone(),
                suggested_evidence: item.suggested_evidence.clone(),
                source_excerpt: Some(item.source_excerpt.clone()),
                linked_item_ids: Vec::new(),
            });
        }
        for scenario in &interactions.scenarios {
            if !ids.insert(scenario.id.clone()) {
                bail!("duplicate acceptance ledger id {:?}", scenario.id);
            }
            entries.push(AcceptanceLedgerEntry {
                id: scenario.id.clone(),
                kind: AcceptanceLedgerEntryKind::Interaction,
                statement: scenario.risk.clone(),
                suggested_evidence: scenario.suggested_evidence.clone(),
                source_excerpt: None,
                linked_item_ids: scenario.item_ids.clone(),
            });
        }
        Ok(Self {
            schema_version: ACCEPTANCE_LEDGER_SCHEMA_VERSION.to_string(),
            entries,
        })
    }

    pub fn render_worker_packet(&self) -> String {
        let rendered = self
            .entries
            .iter()
            .map(|entry| {
                let kind = match entry.kind {
                    AcceptanceLedgerEntryKind::Requirement => "requirement",
                    AcceptanceLedgerEntryKind::Interaction => "interaction",
                };
                let linked = if entry.linked_item_ids.is_empty() {
                    String::new()
                } else {
                    format!("\n  Links: {}", entry.linked_item_ids.join(", "))
                };
                let source = entry
                    .source_excerpt
                    .as_ref()
                    .map(|source| format!("\n  Public source: {source}"))
                    .unwrap_or_default();
                format!(
                    "- [{}] {}: {}{}{}\n  Suggested evidence: {}",
                    entry.id, kind, entry.statement, linked, source, entry.suggested_evidence
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "Acceptance coverage ledger (advisory, not validation authority):\n\
             {rendered}\n\n\
             Address every entry, including combined interaction scenarios. After the latest relevant mutation, use `submit_acceptance_evidence` to cite the deterministic observations that cover these IDs. You may cover several IDs with one evidence submission. A submission records coverage only; it does not replace declared probes or prove semantic correctness. DONE is withheld while any ledger entry lacks current coverage."
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcceptanceEvidenceRecord {
    pub mutation_epoch: usize,
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcceptanceLedgerSnapshot {
    pub schema_version: String,
    pub mutation_epoch: usize,
    pub entries: Vec<AcceptanceLedgerEntry>,
    pub evidence: BTreeMap<String, AcceptanceEvidenceRecord>,
    pub incomplete_ids: Vec<String>,
}

impl AcceptanceLedgerSnapshot {
    pub fn is_complete(&self) -> bool {
        self.incomplete_ids.is_empty()
    }
}

#[derive(Debug, Clone, Default)]
pub struct AcceptanceLedgerState {
    spec: Option<AcceptanceLedgerSpec>,
    evidence: BTreeMap<String, AcceptanceEvidenceRecord>,
}

impl AcceptanceLedgerState {
    pub fn configure(&mut self, spec: AcceptanceLedgerSpec) -> Result<()> {
        if self.spec.is_some() || !self.evidence.is_empty() {
            bail!("acceptance ledger is already configured");
        }
        if spec.entries.is_empty() {
            bail!("acceptance ledger must contain at least one entry");
        }
        self.spec = Some(spec);
        Ok(())
    }

    pub fn is_configured(&self) -> bool {
        self.spec.is_some()
    }

    pub fn record(
        &mut self,
        ids: &[String],
        evidence: &str,
        mutation_epoch: usize,
    ) -> Result<AcceptanceLedgerSnapshot> {
        let Some(spec) = &self.spec else {
            bail!("acceptance ledger is not configured");
        };
        if ids.is_empty() {
            bail!("acceptance_ids must contain at least one ledger id");
        }
        let evidence = evidence.trim();
        if evidence.is_empty() {
            bail!("evidence must not be empty");
        }
        if evidence.chars().count() > MAX_ACCEPTANCE_EVIDENCE_CHARS {
            bail!(
                "evidence exceeds the {} character limit",
                MAX_ACCEPTANCE_EVIDENCE_CHARS
            );
        }
        let known = spec
            .entries
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<HashSet<_>>();
        let mut seen = HashSet::new();
        for id in ids {
            if !known.contains(id.as_str()) {
                bail!("unknown acceptance ledger id {id:?}");
            }
            if !seen.insert(id.as_str()) {
                bail!("duplicate acceptance ledger id {id:?} in submission");
            }
        }
        for id in ids {
            self.evidence.insert(
                id.clone(),
                AcceptanceEvidenceRecord {
                    mutation_epoch,
                    evidence: evidence.to_string(),
                },
            );
        }
        Ok(self.snapshot(mutation_epoch))
    }

    pub fn snapshot(&self, mutation_epoch: usize) -> AcceptanceLedgerSnapshot {
        let entries = self
            .spec
            .as_ref()
            .map(|spec| spec.entries.clone())
            .unwrap_or_default();
        let incomplete_ids = entries
            .iter()
            .filter(|entry| {
                self.evidence
                    .get(&entry.id)
                    .is_none_or(|record| record.mutation_epoch != mutation_epoch)
            })
            .map(|entry| entry.id.clone())
            .collect();
        AcceptanceLedgerSnapshot {
            schema_version: ACCEPTANCE_LEDGER_SCHEMA_VERSION.to_string(),
            mutation_epoch,
            entries,
            evidence: self.evidence.clone(),
            incomplete_ids,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acceptance_interactions::{
        ACCEPTANCE_INTERACTIONS_SCHEMA_VERSION, AcceptanceInteractionScenario,
    };
    use crate::acceptance_plan::{
        ACCEPTANCE_PLAN_SCHEMA_VERSION, AcceptanceItem, AcceptanceItemKind,
    };

    fn spec() -> AcceptanceLedgerSpec {
        AcceptanceLedgerSpec::from_plans(
            &AcceptancePlan {
                schema_version: ACCEPTANCE_PLAN_SCHEMA_VERSION.to_string(),
                items: vec![AcceptanceItem {
                    id: "req-order".into(),
                    requirement: "Preserve selected order.".into(),
                    kind: AcceptanceItemKind::Behavior,
                    source_excerpt: "include fixes raw feature order".into(),
                    suggested_evidence: "Observe the selected order.".into(),
                }],
            },
            &AcceptanceInteractions {
                schema_version: ACCEPTANCE_INTERACTIONS_SCHEMA_VERSION.to_string(),
                scenarios: vec![AcceptanceInteractionScenario {
                    id: "interaction-remove".into(),
                    item_ids: vec!["req-order".into(), "req-exclude".into()],
                    risk: "Removal may corrupt order.".into(),
                    suggested_evidence: "Include then exclude the same entry.".into(),
                }],
            },
        )
        .unwrap()
    }

    #[test]
    fn current_evidence_completes_and_mutation_makes_it_stale() {
        let mut state = AcceptanceLedgerState::default();
        state.configure(spec()).unwrap();
        let ids = vec!["req-order".into(), "interaction-remove".into()];
        assert!(
            state
                .record(&ids, "focused test passed", 2)
                .unwrap()
                .is_complete()
        );
        assert!(!state.snapshot(3).is_complete());
    }

    #[test]
    fn rejects_unknown_and_duplicate_ids() {
        let mut state = AcceptanceLedgerState::default();
        state.configure(spec()).unwrap();
        assert!(state.record(&["missing".into()], "evidence", 0).is_err());
        assert!(
            state
                .record(&["req-order".into(), "req-order".into()], "evidence", 0)
                .is_err()
        );
    }

    #[test]
    fn rendered_packet_labels_advisory_coverage_and_keeps_public_source() {
        let packet = spec().render_worker_packet();
        assert!(packet.contains("advisory, not validation authority"));
        assert!(packet.contains("include fixes raw feature order"));
        assert!(packet.contains("Include then exclude the same entry"));
        assert!(packet.contains("submit_acceptance_evidence"));
    }
}
