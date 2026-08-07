//! Proposal-only acceptance planning before the worker loop.
//!
//! A bounded semantic advisory decomposes public task guidance into observable
//! acceptance items. Deterministic policy validates shape, size, identity, and
//! verbatim provenance. The resulting plan is measurement only: it has no
//! authority to mutate runtime state or accept worker completion.

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

pub const ACCEPTANCE_PLAN_SCHEMA_VERSION: &str = "acceptance_plan.v1";
pub const DEFAULT_MAX_PLAN_ITEMS: usize = 16;
pub const DEFAULT_MAX_PLAN_INPUT_CHARS: usize = 16_000;
pub const DEFAULT_MAX_PLAN_OUTPUT_TOKENS: usize = 2_048;
pub const DEFAULT_MAX_PLAN_ATTEMPTS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceptanceItemKind {
    Artifact,
    Behavior,
    Constraint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceItem {
    pub id: String,
    pub requirement: String,
    pub kind: AcceptanceItemKind,
    pub source_excerpt: String,
    pub suggested_evidence: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptancePlan {
    pub schema_version: String,
    pub items: Vec<AcceptanceItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AcceptancePlanViolation {
    WrongSchema { actual: String },
    NoItems,
    TooManyItems { maximum: usize, actual: usize },
    EmptyId { index: usize },
    InvalidId { id: String },
    DuplicateId { id: String },
    EmptyRequirement { id: String },
    EmptySourceExcerpt { id: String },
    SourceExcerptNotInGuidance { id: String },
    EmptySuggestedEvidence { id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcceptancePlanPolicyOutcome {
    pub accepted: bool,
    pub item_count: usize,
    pub violations: Vec<AcceptancePlanViolation>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AcceptancePlanningSummary {
    pub model: String,
    pub trace_file: PathBuf,
    pub duration_ms: u128,
    pub max_output_tokens: usize,
    pub attempts: usize,
    pub plan: AcceptancePlan,
    pub policy: AcceptancePlanPolicyOutcome,
}

pub async fn plan_acceptance(
    model: &str,
    guidance: &str,
    trace_dir: &Path,
    max_items: usize,
    max_output_tokens: usize,
) -> Result<AcceptancePlanningSummary> {
    let gateway = OllamaGateway::new();
    plan_acceptance_with_gateway(
        &gateway,
        model,
        guidance,
        trace_dir,
        max_items,
        max_output_tokens,
    )
    .await
}

pub async fn plan_acceptance_with_gateway<G: LlmGateway + ?Sized>(
    gateway: &G,
    model: &str,
    guidance: &str,
    trace_dir: &Path,
    max_items: usize,
    max_output_tokens: usize,
) -> Result<AcceptancePlanningSummary> {
    let trace = TraceRecorder::create(trace_dir)?;
    let transaction_started = Instant::now();
    let mut messages = planning_messages(guidance, max_items);
    let mut last_error = "no planning attempt completed".to_string();

    for attempt in 1..=DEFAULT_MAX_PLAN_ATTEMPTS {
        let response = request_semantic_advisory(
            gateway,
            SemanticAdvisoryRequest {
                advisory_kind: SemanticAdvisoryKind::AcceptancePlanning,
                model,
                messages: &messages,
                response_schema: planning_schema(max_items),
                max_input_chars: DEFAULT_MAX_PLAN_INPUT_CHARS,
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
                trace_plan_attempt(&trace, attempt, "advisory_failed", Some(&last_error))?;
                continue;
            }
        };
        let raw_proposal = response.raw_proposal;
        let shape_violations = proposal_shape_violations(&raw_proposal, max_items);
        if !shape_violations.is_empty() {
            last_error = serde_json::to_string(&shape_violations)?;
            trace_plan_attempt(&trace, attempt, "decode_failed", Some(&last_error))?;
            messages = repair_messages(guidance, max_items, &raw_proposal, &last_error)?;
            continue;
        }
        let plan = match serde_json::from_value::<AcceptancePlan>(raw_proposal.clone()) {
            Ok(plan) => plan,
            Err(error) => {
                last_error = format!("decoding acceptance planning proposal: {error}");
                trace_plan_attempt(&trace, attempt, "decode_failed", Some(&last_error))?;
                messages = repair_messages(guidance, max_items, &raw_proposal, &last_error)?;
                continue;
            }
        };
        let policy = evaluate_plan(guidance, &plan, max_items);
        trace.event(
            crate::runtime_events::ACCEPTANCE_PLAN_POLICY_EVALUATED,
            json!({
                "schema_version": ACCEPTANCE_PLAN_SCHEMA_VERSION,
                "attempt": attempt,
                "plan": &plan,
                "policy": &policy,
                "authority": "measurement_only",
            }),
        )?;
        if policy.accepted {
            trace_plan_attempt(&trace, attempt, "accepted", None)?;
            return Ok(AcceptancePlanningSummary {
                model: model.to_string(),
                trace_file: trace.path().to_path_buf(),
                duration_ms: transaction_started.elapsed().as_millis(),
                max_output_tokens,
                attempts: attempt,
                plan,
                policy,
            });
        }

        last_error = serde_json::to_string(&policy.violations)?;
        trace_plan_attempt(&trace, attempt, "policy_rejected", Some(&last_error))?;
        messages = repair_messages(guidance, max_items, &raw_proposal, &last_error)?;
    }

    bail!(
        "acceptance planning exhausted {} attempts: {last_error}",
        DEFAULT_MAX_PLAN_ATTEMPTS
    )
}

fn proposal_shape_violations(proposal: &Value, max_items: usize) -> Vec<String> {
    let Some(root) = proposal.as_object() else {
        return vec!["root must be a JSON object".to_string()];
    };
    let mut violations = Vec::new();
    let root_fields = ["schema_version", "items"];
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
        && version.as_str() != Some(ACCEPTANCE_PLAN_SCHEMA_VERSION)
    {
        violations.push(format!(
            "schema_version must equal `{ACCEPTANCE_PLAN_SCHEMA_VERSION}`"
        ));
    }

    let Some(items) = root.get("items") else {
        return violations;
    };
    let Some(items) = items.as_array() else {
        violations.push("`items` must be an array".to_string());
        return violations;
    };
    if items.is_empty() {
        violations.push("`items` must contain at least one item".to_string());
    }
    if items.len() > max_items {
        violations.push(format!("`items` must contain at most {max_items} items"));
    }

    let item_fields = [
        "id",
        "requirement",
        "kind",
        "source_excerpt",
        "suggested_evidence",
    ];
    for (index, item) in items.iter().enumerate() {
        let Some(item) = item.as_object() else {
            violations.push(format!("items[{index}] must be a JSON object"));
            continue;
        };
        for field in item_fields {
            match item.get(field) {
                None => violations.push(format!(
                    "items[{index}] is missing required field `{field}`"
                )),
                Some(value) if !value.is_string() => {
                    violations.push(format!("items[{index}].{field} must be a string"))
                }
                Some(_) => {}
            }
        }
        for field in item.keys() {
            if !item_fields.contains(&field.as_str()) {
                violations.push(format!(
                    "items[{index}] has unsupported field `{field}`; remove it"
                ));
            }
        }
        if let Some(kind) = item.get("kind").and_then(Value::as_str)
            && !matches!(kind, "artifact" | "behavior" | "constraint")
        {
            violations.push(format!(
                "items[{index}].kind must be `artifact`, `behavior`, or `constraint`"
            ));
        }
    }
    violations
}

fn trace_plan_attempt(
    trace: &TraceRecorder,
    attempt: usize,
    outcome: &str,
    error: Option<&str>,
) -> Result<()> {
    trace.event(
        crate::runtime_events::ACCEPTANCE_PLAN_ATTEMPT_FINISHED,
        json!({
            "attempt": attempt,
            "maximum_attempts": DEFAULT_MAX_PLAN_ATTEMPTS,
            "outcome": outcome,
            "error": error,
        }),
    )
}

fn repair_messages(
    guidance: &str,
    max_items: usize,
    previous: &Value,
    feedback: &str,
) -> Result<Vec<LlmMessage>> {
    if previous.is_null() {
        return Err(anyhow!("planning repair requires a previous proposal"));
    }
    Ok(vec![
        LlmMessage::system(
            "You are repairing only the protocol shape of an acceptance-plan proposal. Preserve every supported requirement and its meaning. Copy every already-valid field unchanged. Make only the changes named by deterministic feedback; never omit a field that feedback did not reject. The root keys must be exactly `schema_version` and `items`, with schema_version exactly `acceptance_plan.v1`. Every item must have exactly `id`, `requirement`, `kind`, `source_excerpt`, and `suggested_evidence`; kind must be artifact, behavior, or constraint. Remove unsupported fields. Return exactly one JSON object. The first response character must be `{` and the last must be `}`. Do not use Markdown or code fences.",
        ),
        LlmMessage::user(format!(
            "Task guidance:\n{guidance}\n\nPrevious proposal:\n{}\n\nDeterministic validation feedback:\n{feedback}\n\nReturn at most {max_items} canonical items. Every source_excerpt must remain an exact substring of the task guidance.",
            serde_json::to_string_pretty(previous)?
        )),
    ])
}

pub fn evaluate_plan(
    guidance: &str,
    plan: &AcceptancePlan,
    max_items: usize,
) -> AcceptancePlanPolicyOutcome {
    let mut violations = Vec::new();
    if plan.schema_version != ACCEPTANCE_PLAN_SCHEMA_VERSION {
        violations.push(AcceptancePlanViolation::WrongSchema {
            actual: plan.schema_version.clone(),
        });
    }
    if plan.items.is_empty() {
        violations.push(AcceptancePlanViolation::NoItems);
    }
    if plan.items.len() > max_items {
        violations.push(AcceptancePlanViolation::TooManyItems {
            maximum: max_items,
            actual: plan.items.len(),
        });
    }

    let mut ids = HashSet::new();
    for (index, item) in plan.items.iter().enumerate() {
        if item.id.trim().is_empty() {
            violations.push(AcceptancePlanViolation::EmptyId { index });
        } else {
            if !valid_id(&item.id) {
                violations.push(AcceptancePlanViolation::InvalidId {
                    id: item.id.clone(),
                });
            }
            if !ids.insert(item.id.as_str()) {
                violations.push(AcceptancePlanViolation::DuplicateId {
                    id: item.id.clone(),
                });
            }
        }
        if item.requirement.trim().is_empty() {
            violations.push(AcceptancePlanViolation::EmptyRequirement {
                id: item.id.clone(),
            });
        }
        if item.source_excerpt.trim().is_empty() {
            violations.push(AcceptancePlanViolation::EmptySourceExcerpt {
                id: item.id.clone(),
            });
        } else if !guidance.contains(item.source_excerpt.trim()) {
            violations.push(AcceptancePlanViolation::SourceExcerptNotInGuidance {
                id: item.id.clone(),
            });
        }
        if item.suggested_evidence.trim().is_empty() {
            violations.push(AcceptancePlanViolation::EmptySuggestedEvidence {
                id: item.id.clone(),
            });
        }
    }

    AcceptancePlanPolicyOutcome {
        accepted: violations.is_empty(),
        item_count: plan.items.len(),
        violations,
    }
}

fn valid_id(id: &str) -> bool {
    id.len() <= 100
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn planning_messages(guidance: &str, max_items: usize) -> Vec<LlmMessage> {
    vec![
        LlmMessage::system(
            "You are an isolated acceptance-planning advisor. Decompose public task guidance into a small checklist of externally observable required outcomes. Preserve interactions between requirements when those interactions need separate evidence. Do not solve the task, use tools, invent requirements, or claim anything is complete. Each source_excerpt must be copied verbatim from the task guidance. The root keys must be exactly `schema_version` and `items`, with schema_version exactly `acceptance_plan.v1`. Every item must have exactly `id`, `requirement`, `kind`, `source_excerpt`, and `suggested_evidence`; kind must be artifact, behavior, or constraint. Do not use the aliases `checklist`, `description`, or `type`. Return exactly one JSON object. The first response character must be `{` and the last must be `}`. Do not use Markdown or code fences.",
        ),
        LlmMessage::user(format!(
            "Task guidance:\n{guidance}\n\nReturn at most {max_items} required acceptance items. Use stable concise IDs. Classify each item as artifact, behavior, or constraint. suggested_evidence describes a deterministic observation that could verify the requirement; it is a proposal, not authority."
        )),
    ]
}

fn planning_schema(max_items: usize) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["schema_version", "items"],
        "properties": {
            "schema_version": {
                "type": "string",
                "const": ACCEPTANCE_PLAN_SCHEMA_VERSION
            },
            "items": {
                "type": "array",
                "minItems": 1,
                "maxItems": max_items,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": [
                        "id",
                        "requirement",
                        "kind",
                        "source_excerpt",
                        "suggested_evidence"
                    ],
                    "properties": {
                        "id": { "type": "string" },
                        "requirement": { "type": "string" },
                        "kind": {
                            "type": "string",
                            "enum": ["artifact", "behavior", "constraint"]
                        },
                        "source_excerpt": { "type": "string" },
                        "suggested_evidence": { "type": "string" }
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use mojentic::MojenticError;
    use mojentic::llm::CompletionConfig;
    use mojentic::llm::gateway::StreamChunk;
    use mojentic::llm::models::LlmGatewayResponse;
    use mojentic::llm::tools::LlmTool;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    struct ProposalGateway {
        proposals: Mutex<VecDeque<Value>>,
        calls: Mutex<usize>,
    }

    impl ProposalGateway {
        fn new(proposals: Vec<Value>) -> Self {
            Self {
                proposals: Mutex::new(proposals.into()),
                calls: Mutex::new(0),
            }
        }
    }

    #[async_trait]
    impl LlmGateway for ProposalGateway {
        async fn complete(
            &self,
            _model: &str,
            messages: &[LlmMessage],
            _tools: Option<&[Box<dyn LlmTool>]>,
            config: &CompletionConfig,
        ) -> std::result::Result<LlmGatewayResponse, MojenticError> {
            assert_eq!(config.max_tool_iterations, 0);
            let system = messages[0].content.as_deref().unwrap();
            assert!(system.contains("isolated") || system.contains("repairing only"));
            assert!(
                messages[1]
                    .content
                    .as_deref()
                    .unwrap()
                    .contains("Task guidance:")
            );
            *self.calls.lock().unwrap() += 1;
            let proposal = self
                .proposals
                .lock()
                .unwrap()
                .pop_front()
                .expect("scripted planning proposal");
            Ok(LlmGatewayResponse {
                content: Some(serde_json::to_string(&proposal).unwrap()),
                object: None,
                tool_calls: Vec::new(),
                thinking: Some("brief reasoning".into()),
            })
        }

        async fn complete_json(
            &self,
            _model: &str,
            messages: &[LlmMessage],
            _schema: Value,
            config: &CompletionConfig,
        ) -> std::result::Result<Value, MojenticError> {
            let _ = (messages, config);
            unreachable!("acceptance planning captures reasoning through complete")
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

    fn accepted_plan() -> AcceptancePlan {
        AcceptancePlan {
            schema_version: ACCEPTANCE_PLAN_SCHEMA_VERSION.into(),
            items: vec![AcceptanceItem {
                id: "preserve_output".into(),
                requirement: "Preserve the requested output.".into(),
                kind: AcceptanceItemKind::Behavior,
                source_excerpt: "Preserve the requested output.".into(),
                suggested_evidence: "Compare observed output with the request.".into(),
            }],
        }
    }

    #[test]
    fn accepts_bounded_plan_with_verbatim_provenance() {
        let plan = accepted_plan();
        let outcome = evaluate_plan("Preserve the requested output.", &plan, 4);
        assert!(outcome.accepted);
        assert!(outcome.violations.is_empty());
    }

    #[test]
    fn rejects_untraceable_and_duplicate_items() {
        let mut plan = accepted_plan();
        plan.items.push(AcceptanceItem {
            id: "preserve_output".into(),
            requirement: "Invented requirement".into(),
            kind: AcceptanceItemKind::Constraint,
            source_excerpt: "This text was not supplied".into(),
            suggested_evidence: "Observe it.".into(),
        });
        let outcome = evaluate_plan("Preserve the requested output.", &plan, 4);
        assert!(!outcome.accepted);
        assert!(
            outcome
                .violations
                .contains(&AcceptancePlanViolation::DuplicateId {
                    id: "preserve_output".into()
                })
        );
        assert!(outcome.violations.contains(
            &AcceptancePlanViolation::SourceExcerptNotInGuidance {
                id: "preserve_output".into()
            }
        ));
    }

    #[test]
    fn shape_feedback_reports_all_missing_extra_and_constant_errors() {
        let proposal = json!({
            "schema_version": "semantic_advisory.v1",
            "items": [{
                "id": "A",
                "description": "wrong alias",
                "type": "artifact",
                "suggested_evidence": "inspect"
            }],
            "checklist": []
        });

        let violations = proposal_shape_violations(&proposal, 16);

        assert!(
            violations
                .iter()
                .any(|entry| entry.contains("schema_version must"))
        );
        assert!(
            violations
                .iter()
                .any(|entry| entry.contains("root has unsupported field `checklist`"))
        );
        assert!(
            violations
                .iter()
                .any(|entry| entry.contains("missing required field `requirement`"))
        );
        assert!(
            violations
                .iter()
                .any(|entry| entry.contains("missing required field `kind`"))
        );
        assert!(
            violations
                .iter()
                .any(|entry| entry.contains("missing required field `source_excerpt`"))
        );
        assert!(
            violations
                .iter()
                .any(|entry| entry.contains("unsupported field `description`"))
        );
        assert!(
            violations
                .iter()
                .any(|entry| entry.contains("unsupported field `type`"))
        );
    }

    #[tokio::test]
    async fn planning_is_measurement_only_and_traceable() {
        let temp = tempfile::tempdir().unwrap();
        let plan = accepted_plan();
        let gateway = ProposalGateway::new(vec![serde_json::to_value(&plan).unwrap()]);
        let summary = plan_acceptance_with_gateway(
            &gateway,
            "small-model",
            "Preserve the requested output.",
            temp.path(),
            4,
            64,
        )
        .await
        .unwrap();

        assert!(summary.policy.accepted);
        assert_eq!(summary.attempts, 1);
        assert_eq!(*gateway.calls.lock().unwrap(), 1);
        let events = std::fs::read_to_string(summary.trace_file).unwrap();
        assert!(events.contains("\"advisory_kind\":\"acceptance_planning\""));
        assert!(events.contains("acceptance_plan.policy_evaluated"));
        assert!(events.contains("\"authority\":\"measurement_only\""));
        assert!(events.contains("\"capture_reasoning\":true"));
        assert!(events.contains("\"thinking_chars\":15"));
    }

    #[tokio::test]
    async fn planning_repairs_a_decodable_wrong_shape() {
        let temp = tempfile::tempdir().unwrap();
        let gateway = ProposalGateway::new(vec![
            json!({"checklist": [{"description": "wrong shape"}]}),
            serde_json::to_value(accepted_plan()).unwrap(),
        ]);
        let summary = plan_acceptance_with_gateway(
            &gateway,
            "small-model",
            "Preserve the requested output.",
            temp.path(),
            4,
            64,
        )
        .await
        .unwrap();

        assert_eq!(summary.attempts, 2);
        assert_eq!(*gateway.calls.lock().unwrap(), 2);
        let events = std::fs::read_to_string(summary.trace_file).unwrap();
        assert!(events.contains("\"outcome\":\"decode_failed\""));
        assert!(events.contains("\"outcome\":\"accepted\""));
    }
}
