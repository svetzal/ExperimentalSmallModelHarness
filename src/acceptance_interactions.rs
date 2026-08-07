//! Proposal-only interaction planning over a validated acceptance plan.
//!
//! Atomic requirements are fixed input. A bounded semantic advisory proposes
//! combined risk scenarios, and deterministic policy validates identity,
//! linkage, and structure. Results remain measurement only.

use crate::acceptance_plan::{AcceptancePlan, DEFAULT_MAX_PLAN_ATTEMPTS, evaluate_plan};
use crate::semantic_advisory::{
    SemanticAdvisoryKind, SemanticAdvisoryRequest, request_semantic_advisory,
};
use crate::trace::TraceRecorder;
use anyhow::{Result, anyhow, bail};
use mojentic::llm::LlmGateway;
use mojentic::llm::gateways::OllamaGateway;
use mojentic::llm::models::LlmMessage;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub const ACCEPTANCE_INTERACTIONS_SCHEMA_VERSION: &str = "acceptance_interactions.v1";
pub const DEFAULT_MAX_INTERACTION_SCENARIOS: usize = 12;
pub const DEFAULT_MAX_INTERACTION_INPUT_CHARS: usize = 16_000;
pub const DEFAULT_MAX_INTERACTION_OUTPUT_TOKENS: usize = 2_048;
pub const ACCEPTANCE_INTERACTION_CANDIDATE_SCHEMA_VERSION: &str =
    "acceptance_interaction_candidate.v1";
pub const ACCEPTANCE_INTERACTION_EVIDENCE_SCHEMA_VERSION: &str =
    "acceptance_interaction_evidence.v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceInteractionScenario {
    pub id: String,
    pub item_ids: Vec<String>,
    pub risk: String,
    pub suggested_evidence: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceInteractions {
    pub schema_version: String,
    pub scenarios: Vec<AcceptanceInteractionScenario>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceInteractionCandidate {
    pub schema_version: String,
    pub id: String,
    pub item_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceInteractionEvidence {
    pub schema_version: String,
    pub candidate_id: String,
    pub item_ids: Vec<String>,
    pub risk: String,
    pub suggested_evidence: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AcceptanceInteractionEvidenceViolation {
    WrongSchema {
        actual: String,
    },
    WrongCandidateId {
        expected: String,
        actual: String,
    },
    WrongItemIds {
        expected: Vec<String>,
        actual: Vec<String>,
    },
    DuplicateItemId {
        item_id: String,
    },
    EmptyRisk,
    EmptySuggestedEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcceptanceInteractionEvidencePolicyOutcome {
    pub accepted: bool,
    pub violations: Vec<AcceptanceInteractionEvidenceViolation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AcceptanceInteractionViolation {
    WrongSchema {
        actual: String,
    },
    NoScenarios,
    TooManyScenarios {
        maximum: usize,
        actual: usize,
    },
    EmptyId {
        index: usize,
    },
    InvalidId {
        id: String,
    },
    DuplicateId {
        id: String,
    },
    TooFewItemIds {
        id: String,
    },
    DuplicateItemId {
        scenario_id: String,
        item_id: String,
    },
    UnknownItemId {
        scenario_id: String,
        item_id: String,
    },
    EmptyRisk {
        id: String,
    },
    EmptySuggestedEvidence {
        id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcceptanceInteractionPolicyOutcome {
    pub accepted: bool,
    pub scenario_count: usize,
    pub violations: Vec<AcceptanceInteractionViolation>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AcceptanceInteractionPlanningSummary {
    pub model: String,
    pub trace_file: PathBuf,
    pub duration_ms: u128,
    pub max_output_tokens: usize,
    pub attempts: usize,
    pub interactions: AcceptanceInteractions,
    pub policy: AcceptanceInteractionPolicyOutcome,
}

#[derive(Debug, Clone, Serialize)]
pub struct AcceptanceInteractionEvidenceSummary {
    pub model: String,
    pub trace_file: PathBuf,
    pub duration_ms: u128,
    pub max_output_tokens: usize,
    pub attempts: usize,
    pub evidence: AcceptanceInteractionEvidence,
    pub policy: AcceptanceInteractionEvidencePolicyOutcome,
}

pub async fn plan_acceptance_interactions(
    model: &str,
    guidance: &str,
    plan: &AcceptancePlan,
    trace_dir: &Path,
    max_scenarios: usize,
    max_output_tokens: usize,
) -> Result<AcceptanceInteractionPlanningSummary> {
    let gateway = OllamaGateway::new();
    plan_acceptance_interactions_with_gateway(
        &gateway,
        model,
        guidance,
        plan,
        trace_dir,
        max_scenarios,
        max_output_tokens,
    )
    .await
}

pub async fn plan_acceptance_interactions_with_gateway<G: LlmGateway + ?Sized>(
    gateway: &G,
    model: &str,
    guidance: &str,
    plan: &AcceptancePlan,
    trace_dir: &Path,
    max_scenarios: usize,
    max_output_tokens: usize,
) -> Result<AcceptanceInteractionPlanningSummary> {
    let plan_policy = evaluate_plan(guidance, plan, plan.items.len().max(1));
    if !plan_policy.accepted {
        bail!(
            "interaction planning requires a valid atomic plan: {}",
            serde_json::to_string(&plan_policy.violations)?
        );
    }

    let trace = TraceRecorder::create(trace_dir)?;
    let transaction_started = Instant::now();
    let mut messages = interaction_messages(guidance, plan, max_scenarios)?;
    let mut last_error = "no interaction-planning attempt completed".to_string();

    for attempt in 1..=DEFAULT_MAX_PLAN_ATTEMPTS {
        let response = request_semantic_advisory(
            gateway,
            SemanticAdvisoryRequest {
                advisory_kind: SemanticAdvisoryKind::AcceptanceInteractionPlanning,
                model,
                messages: &messages,
                response_schema: interaction_schema(max_scenarios, plan.items.len()),
                max_input_chars: DEFAULT_MAX_INTERACTION_INPUT_CHARS,
                max_output_tokens,
                temperature: 0.2,
                capture_reasoning: true,
            },
            &trace,
        )
        .await;
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                last_error = error.to_string();
                trace_attempt(&trace, attempt, "advisory_failed", Some(&last_error))?;
                continue;
            }
        };
        let raw_proposal = response.raw_proposal;
        let shape_violations = shape_violations(&raw_proposal, max_scenarios);
        if !shape_violations.is_empty() {
            last_error = serde_json::to_string(&shape_violations)?;
            trace_attempt(&trace, attempt, "decode_failed", Some(&last_error))?;
            messages = repair_messages(guidance, plan, max_scenarios, &raw_proposal, &last_error)?;
            continue;
        }
        let interactions =
            match serde_json::from_value::<AcceptanceInteractions>(raw_proposal.clone()) {
                Ok(interactions) => interactions,
                Err(error) => {
                    last_error = format!("decoding interaction proposal: {error}");
                    trace_attempt(&trace, attempt, "decode_failed", Some(&last_error))?;
                    messages =
                        repair_messages(guidance, plan, max_scenarios, &raw_proposal, &last_error)?;
                    continue;
                }
            };
        let policy = evaluate_interactions(plan, &interactions, max_scenarios);
        trace.event(
            crate::runtime_events::ACCEPTANCE_INTERACTIONS_POLICY_EVALUATED,
            json!({
                "schema_version": ACCEPTANCE_INTERACTIONS_SCHEMA_VERSION,
                "attempt": attempt,
                "interactions": &interactions,
                "policy": &policy,
                "authority": "measurement_only",
            }),
        )?;
        if policy.accepted {
            trace_attempt(&trace, attempt, "accepted", None)?;
            return Ok(AcceptanceInteractionPlanningSummary {
                model: model.to_string(),
                trace_file: trace.path().to_path_buf(),
                duration_ms: transaction_started.elapsed().as_millis(),
                max_output_tokens,
                attempts: attempt,
                interactions,
                policy,
            });
        }

        last_error = serde_json::to_string(&policy.violations)?;
        trace_attempt(&trace, attempt, "policy_rejected", Some(&last_error))?;
        messages = repair_messages(guidance, plan, max_scenarios, &raw_proposal, &last_error)?;
    }

    bail!(
        "interaction planning exhausted {} attempts: {last_error}",
        DEFAULT_MAX_PLAN_ATTEMPTS
    )
}

pub async fn author_interaction_evidence(
    model: &str,
    guidance: &str,
    plan: &AcceptancePlan,
    candidate: &AcceptanceInteractionCandidate,
    trace_dir: &Path,
    max_output_tokens: usize,
) -> Result<AcceptanceInteractionEvidenceSummary> {
    let gateway = OllamaGateway::new();
    author_interaction_evidence_with_gateway(
        &gateway,
        model,
        guidance,
        plan,
        candidate,
        trace_dir,
        max_output_tokens,
    )
    .await
}

pub async fn author_interaction_evidence_with_gateway<G: LlmGateway + ?Sized>(
    gateway: &G,
    model: &str,
    guidance: &str,
    plan: &AcceptancePlan,
    candidate: &AcceptanceInteractionCandidate,
    trace_dir: &Path,
    max_output_tokens: usize,
) -> Result<AcceptanceInteractionEvidenceSummary> {
    validate_candidate(guidance, plan, candidate)?;
    let trace = TraceRecorder::create(trace_dir)?;
    let transaction_started = Instant::now();
    let mut messages = evidence_messages(guidance, plan, candidate)?;
    let mut last_error = "no evidence-authoring attempt completed".to_string();

    for attempt in 1..=DEFAULT_MAX_PLAN_ATTEMPTS {
        let response = request_semantic_advisory(
            gateway,
            SemanticAdvisoryRequest {
                advisory_kind: SemanticAdvisoryKind::AcceptanceInteractionEvidence,
                model,
                messages: &messages,
                response_schema: evidence_schema(plan.items.len()),
                max_input_chars: DEFAULT_MAX_INTERACTION_INPUT_CHARS,
                max_output_tokens,
                temperature: 0.2,
                capture_reasoning: true,
            },
            &trace,
        )
        .await;
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                last_error = error.to_string();
                trace_evidence_attempt(&trace, attempt, "advisory_failed", Some(&last_error))?;
                continue;
            }
        };
        let raw_proposal = response.raw_proposal;
        let shape_violations = evidence_shape_violations(&raw_proposal);
        if !shape_violations.is_empty() {
            last_error = serde_json::to_string(&shape_violations)?;
            trace_evidence_attempt(&trace, attempt, "decode_failed", Some(&last_error))?;
            messages =
                evidence_repair_messages(guidance, plan, candidate, &raw_proposal, &last_error)?;
            continue;
        }
        let evidence =
            match serde_json::from_value::<AcceptanceInteractionEvidence>(raw_proposal.clone()) {
                Ok(evidence) => evidence,
                Err(error) => {
                    last_error = format!("decoding supplied-interaction evidence: {error}");
                    trace_evidence_attempt(&trace, attempt, "decode_failed", Some(&last_error))?;
                    messages = evidence_repair_messages(
                        guidance,
                        plan,
                        candidate,
                        &raw_proposal,
                        &last_error,
                    )?;
                    continue;
                }
            };
        let policy = evaluate_evidence(candidate, &evidence);
        trace.event(
            crate::runtime_events::ACCEPTANCE_INTERACTION_EVIDENCE_POLICY_EVALUATED,
            json!({
                "schema_version": ACCEPTANCE_INTERACTION_EVIDENCE_SCHEMA_VERSION,
                "attempt": attempt,
                "candidate": candidate,
                "evidence": &evidence,
                "policy": &policy,
                "authority": "measurement_only",
            }),
        )?;
        if policy.accepted {
            trace_evidence_attempt(&trace, attempt, "accepted", None)?;
            return Ok(AcceptanceInteractionEvidenceSummary {
                model: model.to_string(),
                trace_file: trace.path().to_path_buf(),
                duration_ms: transaction_started.elapsed().as_millis(),
                max_output_tokens,
                attempts: attempt,
                evidence,
                policy,
            });
        }

        last_error = serde_json::to_string(&policy.violations)?;
        trace_evidence_attempt(&trace, attempt, "policy_rejected", Some(&last_error))?;
        messages = evidence_repair_messages(guidance, plan, candidate, &raw_proposal, &last_error)?;
    }

    bail!(
        "interaction evidence exhausted {} attempts: {last_error}",
        DEFAULT_MAX_PLAN_ATTEMPTS
    )
}

pub fn evaluate_evidence(
    candidate: &AcceptanceInteractionCandidate,
    evidence: &AcceptanceInteractionEvidence,
) -> AcceptanceInteractionEvidencePolicyOutcome {
    let mut violations = Vec::new();
    if evidence.schema_version != ACCEPTANCE_INTERACTION_EVIDENCE_SCHEMA_VERSION {
        violations.push(AcceptanceInteractionEvidenceViolation::WrongSchema {
            actual: evidence.schema_version.clone(),
        });
    }
    if evidence.candidate_id != candidate.id {
        violations.push(AcceptanceInteractionEvidenceViolation::WrongCandidateId {
            expected: candidate.id.clone(),
            actual: evidence.candidate_id.clone(),
        });
    }
    let expected = candidate.item_ids.iter().cloned().collect::<HashSet<_>>();
    let actual = evidence.item_ids.iter().cloned().collect::<HashSet<_>>();
    if expected != actual || evidence.item_ids.len() != candidate.item_ids.len() {
        violations.push(AcceptanceInteractionEvidenceViolation::WrongItemIds {
            expected: candidate.item_ids.clone(),
            actual: evidence.item_ids.clone(),
        });
    }
    let mut seen = HashSet::new();
    for item_id in &evidence.item_ids {
        if !seen.insert(item_id.as_str()) {
            violations.push(AcceptanceInteractionEvidenceViolation::DuplicateItemId {
                item_id: item_id.clone(),
            });
        }
    }
    if evidence.risk.trim().is_empty() {
        violations.push(AcceptanceInteractionEvidenceViolation::EmptyRisk);
    }
    if evidence.suggested_evidence.trim().is_empty() {
        violations.push(AcceptanceInteractionEvidenceViolation::EmptySuggestedEvidence);
    }
    AcceptanceInteractionEvidencePolicyOutcome {
        accepted: violations.is_empty(),
        violations,
    }
}

fn validate_candidate(
    guidance: &str,
    plan: &AcceptancePlan,
    candidate: &AcceptanceInteractionCandidate,
) -> Result<()> {
    let plan_policy = evaluate_plan(guidance, plan, plan.items.len().max(1));
    if !plan_policy.accepted {
        bail!(
            "interaction evidence requires a valid atomic plan: {}",
            serde_json::to_string(&plan_policy.violations)?
        );
    }
    if candidate.schema_version != ACCEPTANCE_INTERACTION_CANDIDATE_SCHEMA_VERSION {
        bail!(
            "candidate schema_version must equal `{ACCEPTANCE_INTERACTION_CANDIDATE_SCHEMA_VERSION}`"
        );
    }
    if candidate.id.trim().is_empty() || !valid_id(&candidate.id) {
        bail!("candidate id must be a non-empty stable ID");
    }
    if candidate.item_ids.len() < 2 {
        bail!("candidate must link at least two item IDs");
    }
    let known_ids = plan
        .items
        .iter()
        .map(|item| item.id.as_str())
        .collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    for item_id in &candidate.item_ids {
        if !seen.insert(item_id.as_str()) {
            bail!("candidate contains duplicate item ID `{item_id}`");
        }
        if !known_ids.contains(item_id.as_str()) {
            bail!("candidate contains unknown item ID `{item_id}`");
        }
    }
    Ok(())
}

pub fn evaluate_interactions(
    plan: &AcceptancePlan,
    interactions: &AcceptanceInteractions,
    max_scenarios: usize,
) -> AcceptanceInteractionPolicyOutcome {
    let mut violations = Vec::new();
    if interactions.schema_version != ACCEPTANCE_INTERACTIONS_SCHEMA_VERSION {
        violations.push(AcceptanceInteractionViolation::WrongSchema {
            actual: interactions.schema_version.clone(),
        });
    }
    if interactions.scenarios.is_empty() {
        violations.push(AcceptanceInteractionViolation::NoScenarios);
    }
    if interactions.scenarios.len() > max_scenarios {
        violations.push(AcceptanceInteractionViolation::TooManyScenarios {
            maximum: max_scenarios,
            actual: interactions.scenarios.len(),
        });
    }

    let known_ids = plan
        .items
        .iter()
        .map(|item| item.id.as_str())
        .collect::<HashSet<_>>();
    let mut scenario_ids = HashSet::new();
    for (index, scenario) in interactions.scenarios.iter().enumerate() {
        if scenario.id.trim().is_empty() {
            violations.push(AcceptanceInteractionViolation::EmptyId { index });
        } else {
            if !valid_id(&scenario.id) {
                violations.push(AcceptanceInteractionViolation::InvalidId {
                    id: scenario.id.clone(),
                });
            }
            if !scenario_ids.insert(scenario.id.as_str()) {
                violations.push(AcceptanceInteractionViolation::DuplicateId {
                    id: scenario.id.clone(),
                });
            }
        }
        if scenario.item_ids.len() < 2 {
            violations.push(AcceptanceInteractionViolation::TooFewItemIds {
                id: scenario.id.clone(),
            });
        }
        let mut linked_ids = HashSet::new();
        for item_id in &scenario.item_ids {
            if !linked_ids.insert(item_id.as_str()) {
                violations.push(AcceptanceInteractionViolation::DuplicateItemId {
                    scenario_id: scenario.id.clone(),
                    item_id: item_id.clone(),
                });
            }
            if !known_ids.contains(item_id.as_str()) {
                violations.push(AcceptanceInteractionViolation::UnknownItemId {
                    scenario_id: scenario.id.clone(),
                    item_id: item_id.clone(),
                });
            }
        }
        if scenario.risk.trim().is_empty() {
            violations.push(AcceptanceInteractionViolation::EmptyRisk {
                id: scenario.id.clone(),
            });
        }
        if scenario.suggested_evidence.trim().is_empty() {
            violations.push(AcceptanceInteractionViolation::EmptySuggestedEvidence {
                id: scenario.id.clone(),
            });
        }
    }

    AcceptanceInteractionPolicyOutcome {
        accepted: violations.is_empty(),
        scenario_count: interactions.scenarios.len(),
        violations,
    }
}

fn interaction_messages(
    guidance: &str,
    plan: &AcceptancePlan,
    max_scenarios: usize,
) -> Result<Vec<LlmMessage>> {
    Ok(vec![
        LlmMessage::system(
            "You are an isolated acceptance-interaction advisor. Atomic requirements and their ordinary evidence are already fixed. Identify high-risk combined scenarios where each requirement could pass independently while their interaction still fails. Consider sequencing, limits, interruption, failure, cleanup, and lifecycle boundaries when supported by the inputs. Do not solve the task, use tools, invent requirements, or claim completion. The root keys must be exactly `schema_version` and `scenarios`, with schema_version exactly `acceptance_interactions.v1`. Every scenario must have exactly `id`, `item_ids`, `risk`, and `suggested_evidence`. item_ids must contain at least two distinct IDs copied from the atomic plan. Return exactly one JSON object without Markdown or code fences.",
        ),
        LlmMessage::user(format!(
            "Task guidance:\n{guidance}\n\nValidated atomic plan:\n{}\n\nReturn at most {max_scenarios} necessary interaction scenarios. suggested_evidence must exercise the linked requirements together, not as separate probes.",
            serde_json::to_string_pretty(plan)?
        )),
    ])
}

fn evidence_messages(
    guidance: &str,
    plan: &AcceptancePlan,
    candidate: &AcceptanceInteractionCandidate,
) -> Result<Vec<LlmMessage>> {
    Ok(vec![
        LlmMessage::system(
            "You are an isolated interaction-evidence advisor. Deterministic policy has already selected one exact group of acceptance requirements. Author one risk and one deterministic probe that exercises every supplied item together in a single scenario. Do not split the items into separate probes, change their identity, solve the task, use tools, or claim completion. The root keys must be exactly `schema_version`, `candidate_id`, `item_ids`, `risk`, and `suggested_evidence`, with schema_version exactly `acceptance_interaction_evidence.v1`. Copy candidate_id and every item_id exactly from the supplied candidate. Return exactly one JSON object without Markdown or code fences.",
        ),
        LlmMessage::user(format!(
            "Task guidance:\n{guidance}\n\nValidated atomic plan:\n{}\n\nSupplied interaction candidate:\n{}\n\nThe suggested evidence must state a concrete setup, intervention, and observable outcomes that exercise all candidate items simultaneously.",
            serde_json::to_string_pretty(plan)?,
            serde_json::to_string_pretty(candidate)?
        )),
    ])
}

fn evidence_repair_messages(
    guidance: &str,
    plan: &AcceptancePlan,
    candidate: &AcceptanceInteractionCandidate,
    previous: &Value,
    feedback: &str,
) -> Result<Vec<LlmMessage>> {
    if previous.is_null() {
        return Err(anyhow!("evidence repair requires a previous proposal"));
    }
    Ok(vec![
        LlmMessage::system(
            "Repair only the rejected interaction-evidence proposal. Copy every already-valid field unchanged and make only the changes named by deterministic feedback. The root keys must be exactly `schema_version`, `candidate_id`, `item_ids`, `risk`, and `suggested_evidence`, with schema_version exactly `acceptance_interaction_evidence.v1`. Copy candidate_id and every supplied item_id exactly. Return exactly one JSON object without Markdown or code fences.",
        ),
        LlmMessage::user(format!(
            "Task guidance:\n{guidance}\n\nValidated atomic plan:\n{}\n\nSupplied interaction candidate:\n{}\n\nPrevious proposal:\n{}\n\nDeterministic validation feedback:\n{feedback}\n\nReturn one corrected combined-evidence proposal.",
            serde_json::to_string_pretty(plan)?,
            serde_json::to_string_pretty(candidate)?,
            serde_json::to_string_pretty(previous)?
        )),
    ])
}

fn evidence_shape_violations(proposal: &Value) -> Vec<String> {
    let Some(root) = proposal.as_object() else {
        return vec!["root must be a JSON object".to_string()];
    };
    let mut violations = Vec::new();
    let fields = [
        "schema_version",
        "candidate_id",
        "item_ids",
        "risk",
        "suggested_evidence",
    ];
    for field in fields {
        if !root.contains_key(field) {
            violations.push(format!("root is missing required field `{field}`"));
        }
    }
    for field in root.keys() {
        if !fields.contains(&field.as_str()) {
            violations.push(format!("root has unsupported field `{field}`; remove it"));
        }
    }
    if let Some(version) = root.get("schema_version")
        && version.as_str() != Some(ACCEPTANCE_INTERACTION_EVIDENCE_SCHEMA_VERSION)
    {
        violations.push(format!(
            "schema_version must equal `{ACCEPTANCE_INTERACTION_EVIDENCE_SCHEMA_VERSION}`"
        ));
    }
    for field in [
        "schema_version",
        "candidate_id",
        "risk",
        "suggested_evidence",
    ] {
        if let Some(value) = root.get(field)
            && !value.is_string()
        {
            violations.push(format!("root.{field} must be a string"));
        }
    }
    if let Some(item_ids) = root.get("item_ids") {
        match item_ids.as_array() {
            Some(item_ids) => {
                if item_ids.len() < 2 {
                    violations.push("root.item_ids must contain at least two IDs".to_string());
                }
                for (index, item_id) in item_ids.iter().enumerate() {
                    if !item_id.is_string() {
                        violations.push(format!("root.item_ids[{index}] must be a string"));
                    }
                }
            }
            None => violations.push("root.item_ids must be an array".to_string()),
        }
    }
    violations
}

fn evidence_schema(max_item_ids: usize) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": [
            "schema_version",
            "candidate_id",
            "item_ids",
            "risk",
            "suggested_evidence"
        ],
        "properties": {
            "schema_version": {
                "type": "string",
                "const": ACCEPTANCE_INTERACTION_EVIDENCE_SCHEMA_VERSION
            },
            "candidate_id": { "type": "string" },
            "item_ids": {
                "type": "array",
                "minItems": 2,
                "maxItems": max_item_ids,
                "uniqueItems": true,
                "items": { "type": "string" }
            },
            "risk": { "type": "string" },
            "suggested_evidence": { "type": "string" }
        }
    })
}

fn repair_messages(
    guidance: &str,
    plan: &AcceptancePlan,
    max_scenarios: usize,
    previous: &Value,
    feedback: &str,
) -> Result<Vec<LlmMessage>> {
    if previous.is_null() {
        return Err(anyhow!("interaction repair requires a previous proposal"));
    }
    Ok(vec![
        LlmMessage::system(
            "Repair only the rejected interaction proposal. Copy every already-valid field unchanged and make only the changes named by deterministic feedback. The root keys must be exactly `schema_version` and `scenarios`, with schema_version exactly `acceptance_interactions.v1`. Every scenario must have exactly `id`, `item_ids`, `risk`, and `suggested_evidence`; item_ids must contain at least two distinct IDs from the atomic plan. Return exactly one JSON object without Markdown or code fences.",
        ),
        LlmMessage::user(format!(
            "Task guidance:\n{guidance}\n\nValidated atomic plan:\n{}\n\nPrevious proposal:\n{}\n\nDeterministic validation feedback:\n{feedback}\n\nReturn at most {max_scenarios} corrected interaction scenarios.",
            serde_json::to_string_pretty(plan)?,
            serde_json::to_string_pretty(previous)?
        )),
    ])
}

fn shape_violations(proposal: &Value, max_scenarios: usize) -> Vec<String> {
    let Some(root) = proposal.as_object() else {
        return vec!["root must be a JSON object".to_string()];
    };
    let mut violations = Vec::new();
    let root_fields = ["schema_version", "scenarios"];
    for field in root_fields {
        if !root.contains_key(field) {
            violations.push(format!("root is missing required field `{field}`"));
        }
    }
    for field in root.keys() {
        if !root_fields.contains(&field.as_str()) {
            violations.push(format!("root has unsupported field `{field}`; remove it"));
        }
    }
    if let Some(version) = root.get("schema_version")
        && version.as_str() != Some(ACCEPTANCE_INTERACTIONS_SCHEMA_VERSION)
    {
        violations.push(format!(
            "schema_version must equal `{ACCEPTANCE_INTERACTIONS_SCHEMA_VERSION}`"
        ));
    }
    let Some(scenarios) = root.get("scenarios") else {
        return violations;
    };
    let Some(scenarios) = scenarios.as_array() else {
        violations.push("`scenarios` must be an array".to_string());
        return violations;
    };
    if scenarios.is_empty() {
        violations.push("`scenarios` must contain at least one scenario".to_string());
    }
    if scenarios.len() > max_scenarios {
        violations.push(format!(
            "`scenarios` must contain at most {max_scenarios} scenarios"
        ));
    }
    let fields = ["id", "item_ids", "risk", "suggested_evidence"];
    for (index, scenario) in scenarios.iter().enumerate() {
        let Some(scenario) = scenario.as_object() else {
            violations.push(format!("scenarios[{index}] must be a JSON object"));
            continue;
        };
        for field in fields {
            if !scenario.contains_key(field) {
                violations.push(format!(
                    "scenarios[{index}] is missing required field `{field}`"
                ));
            }
        }
        for field in scenario.keys() {
            if !fields.contains(&field.as_str()) {
                violations.push(format!(
                    "scenarios[{index}] has unsupported field `{field}`; remove it"
                ));
            }
        }
        for field in ["id", "risk", "suggested_evidence"] {
            if let Some(value) = scenario.get(field)
                && !value.is_string()
            {
                violations.push(format!("scenarios[{index}].{field} must be a string"));
            }
        }
        if let Some(item_ids) = scenario.get("item_ids") {
            match item_ids.as_array() {
                Some(item_ids) => {
                    if item_ids.len() < 2 {
                        violations.push(format!(
                            "scenarios[{index}].item_ids must contain at least two IDs"
                        ));
                    }
                    for (item_index, item_id) in item_ids.iter().enumerate() {
                        if !item_id.is_string() {
                            violations.push(format!(
                                "scenarios[{index}].item_ids[{item_index}] must be a string"
                            ));
                        }
                    }
                }
                None => violations.push(format!("scenarios[{index}].item_ids must be an array")),
            }
        }
    }
    violations
}

fn interaction_schema(max_scenarios: usize, max_item_ids: usize) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["schema_version", "scenarios"],
        "properties": {
            "schema_version": {
                "type": "string",
                "const": ACCEPTANCE_INTERACTIONS_SCHEMA_VERSION
            },
            "scenarios": {
                "type": "array",
                "minItems": 1,
                "maxItems": max_scenarios,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["id", "item_ids", "risk", "suggested_evidence"],
                    "properties": {
                        "id": { "type": "string" },
                        "item_ids": {
                            "type": "array",
                            "minItems": 2,
                            "maxItems": max_item_ids,
                            "uniqueItems": true,
                            "items": { "type": "string" }
                        },
                        "risk": { "type": "string" },
                        "suggested_evidence": { "type": "string" }
                    }
                }
            }
        }
    })
}

fn trace_attempt(
    trace: &TraceRecorder,
    attempt: usize,
    outcome: &str,
    error: Option<&str>,
) -> Result<()> {
    trace.event(
        crate::runtime_events::ACCEPTANCE_INTERACTIONS_ATTEMPT_FINISHED,
        json!({
            "attempt": attempt,
            "maximum_attempts": DEFAULT_MAX_PLAN_ATTEMPTS,
            "outcome": outcome,
            "error": error,
        }),
    )
}

fn trace_evidence_attempt(
    trace: &TraceRecorder,
    attempt: usize,
    outcome: &str,
    error: Option<&str>,
) -> Result<()> {
    trace.event(
        crate::runtime_events::ACCEPTANCE_INTERACTION_EVIDENCE_ATTEMPT_FINISHED,
        json!({
            "attempt": attempt,
            "maximum_attempts": DEFAULT_MAX_PLAN_ATTEMPTS,
            "outcome": outcome,
            "error": error,
        }),
    )
}

fn valid_id(id: &str) -> bool {
    id.len() <= 100
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acceptance_plan::{
        ACCEPTANCE_PLAN_SCHEMA_VERSION, AcceptanceItem, AcceptanceItemKind,
    };
    use async_trait::async_trait;
    use mojentic::MojenticError;
    use mojentic::llm::CompletionConfig;
    use mojentic::llm::gateway::StreamChunk;
    use mojentic::llm::models::LlmGatewayResponse;
    use mojentic::llm::tools::LlmTool;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    fn plan() -> AcceptancePlan {
        AcceptancePlan {
            schema_version: ACCEPTANCE_PLAN_SCHEMA_VERSION.into(),
            items: vec![
                AcceptanceItem {
                    id: "limit".into(),
                    requirement: "Limit active work.".into(),
                    kind: AcceptanceItemKind::Behavior,
                    source_excerpt: "Limit active work.".into(),
                    suggested_evidence: "Measure active work.".into(),
                },
                AcceptanceItem {
                    id: "cleanup".into(),
                    requirement: "Cleanup interrupted work.".into(),
                    kind: AcceptanceItemKind::Behavior,
                    source_excerpt: "Cleanup interrupted work.".into(),
                    suggested_evidence: "Interrupt and observe cleanup.".into(),
                },
            ],
        }
    }

    fn accepted_interactions() -> AcceptanceInteractions {
        AcceptanceInteractions {
            schema_version: ACCEPTANCE_INTERACTIONS_SCHEMA_VERSION.into(),
            scenarios: vec![AcceptanceInteractionScenario {
                id: "limit_cleanup".into(),
                item_ids: vec!["limit".into(), "cleanup".into()],
                risk: "Queued work may not clean up.".into(),
                suggested_evidence: "Interrupt excess work and observe cleanup.".into(),
            }],
        }
    }

    fn candidate() -> AcceptanceInteractionCandidate {
        AcceptanceInteractionCandidate {
            schema_version: ACCEPTANCE_INTERACTION_CANDIDATE_SCHEMA_VERSION.into(),
            id: "limit_cleanup".into(),
            item_ids: vec!["limit".into(), "cleanup".into()],
        }
    }

    fn accepted_evidence() -> AcceptanceInteractionEvidence {
        AcceptanceInteractionEvidence {
            schema_version: ACCEPTANCE_INTERACTION_EVIDENCE_SCHEMA_VERSION.into(),
            candidate_id: "limit_cleanup".into(),
            item_ids: vec!["limit".into(), "cleanup".into()],
            risk: "Queued work may not clean up.".into(),
            suggested_evidence: "Interrupt excess work and observe cleanup.".into(),
        }
    }

    struct ProposalGateway {
        proposals: Mutex<VecDeque<Value>>,
    }

    #[async_trait]
    impl LlmGateway for ProposalGateway {
        async fn complete(
            &self,
            _model: &str,
            _messages: &[LlmMessage],
            _tools: Option<&[Box<dyn LlmTool>]>,
            config: &CompletionConfig,
        ) -> std::result::Result<LlmGatewayResponse, MojenticError> {
            assert_eq!(config.max_tool_iterations, 0);
            let proposal = self
                .proposals
                .lock()
                .unwrap()
                .pop_front()
                .expect("scripted interaction proposal");
            Ok(LlmGatewayResponse {
                content: Some(serde_json::to_string(&proposal).unwrap()),
                object: None,
                tool_calls: Vec::new(),
                thinking: Some("interaction reasoning".into()),
            })
        }

        async fn complete_json(
            &self,
            _model: &str,
            _messages: &[LlmMessage],
            _schema: Value,
            _config: &CompletionConfig,
        ) -> std::result::Result<Value, MojenticError> {
            unreachable!("interaction planning captures reasoning through complete")
        }

        async fn get_available_models(&self) -> std::result::Result<Vec<String>, MojenticError> {
            Ok(Vec::new())
        }

        async fn calculate_embeddings(
            &self,
            _text: &str,
            _model: Option<&str>,
        ) -> std::result::Result<Vec<f32>, MojenticError> {
            Ok(Vec::new())
        }

        fn complete_stream<'a>(
            &'a self,
            _model: &'a str,
            _messages: &'a [LlmMessage],
            _tools: Option<&'a [Box<dyn LlmTool>]>,
            _config: &'a CompletionConfig,
        ) -> std::pin::Pin<
            Box<
                dyn futures::Stream<Item = std::result::Result<StreamChunk, MojenticError>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(futures::stream::empty())
        }
    }

    #[test]
    fn policy_rejects_single_unknown_and_duplicate_links() {
        let mut interactions = accepted_interactions();
        interactions.scenarios[0].item_ids = vec!["missing".into(), "missing".into()];

        let outcome = evaluate_interactions(&plan(), &interactions, 4);

        assert!(!outcome.accepted);
        assert!(outcome.violations.iter().any(|violation| matches!(
            violation,
            AcceptanceInteractionViolation::DuplicateItemId { .. }
        )));
        assert!(outcome.violations.iter().any(|violation| matches!(
            violation,
            AcceptanceInteractionViolation::UnknownItemId { .. }
        )));
    }

    #[tokio::test]
    async fn interaction_planning_repairs_shape_and_remains_measurement_only() {
        let temp = tempfile::tempdir().unwrap();
        let gateway = ProposalGateway {
            proposals: Mutex::new(
                vec![
                    json!({"interactions": [{"description": "wrong"}]}),
                    serde_json::to_value(accepted_interactions()).unwrap(),
                ]
                .into(),
            ),
        };

        let summary = plan_acceptance_interactions_with_gateway(
            &gateway,
            "small-model",
            "Limit active work. Cleanup interrupted work.",
            &plan(),
            temp.path(),
            4,
            64,
        )
        .await
        .unwrap();

        assert_eq!(summary.attempts, 2);
        assert!(summary.policy.accepted);
        let events = std::fs::read_to_string(summary.trace_file).unwrap();
        assert!(events.contains("acceptance_interactions.policy_evaluated"));
        assert!(events.contains("\"authority\":\"measurement_only\""));
        assert!(events.contains("\"outcome\":\"decode_failed\""));
        assert!(events.contains("\"thinking_chars\":21"));
    }

    #[test]
    fn supplied_evidence_requires_the_exact_candidate_identity() {
        let mut evidence = accepted_evidence();
        evidence.item_ids = vec!["limit".into(), "missing".into()];

        let outcome = evaluate_evidence(&candidate(), &evidence);

        assert!(!outcome.accepted);
        assert!(outcome.violations.iter().any(|violation| matches!(
            violation,
            AcceptanceInteractionEvidenceViolation::WrongItemIds { .. }
        )));
    }

    #[tokio::test]
    async fn supplied_evidence_repairs_shape_and_remains_measurement_only() {
        let temp = tempfile::tempdir().unwrap();
        let gateway = ProposalGateway {
            proposals: Mutex::new(
                vec![
                    json!({"scenario": {"description": "wrong"}}),
                    serde_json::to_value(accepted_evidence()).unwrap(),
                ]
                .into(),
            ),
        };

        let summary = author_interaction_evidence_with_gateway(
            &gateway,
            "small-model",
            "Limit active work. Cleanup interrupted work.",
            &plan(),
            &candidate(),
            temp.path(),
            64,
        )
        .await
        .unwrap();

        assert_eq!(summary.attempts, 2);
        assert!(summary.policy.accepted);
        let events = std::fs::read_to_string(summary.trace_file).unwrap();
        assert!(events.contains("acceptance_interaction_evidence.policy_evaluated"));
        assert!(events.contains("\"authority\":\"measurement_only\""));
        assert!(events.contains("\"outcome\":\"decode_failed\""));
    }
}
