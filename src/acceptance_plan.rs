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
use anyhow::{Context, Result};
use mojentic::llm::LlmGateway;
use mojentic::llm::gateways::OllamaGateway;
use mojentic::llm::models::LlmMessage;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub const ACCEPTANCE_PLAN_SCHEMA_VERSION: &str = "acceptance_plan.v1";
pub const DEFAULT_MAX_PLAN_ITEMS: usize = 16;
pub const DEFAULT_MAX_PLAN_INPUT_CHARS: usize = 16_000;
pub const DEFAULT_MAX_PLAN_OUTPUT_TOKENS: usize = 2_048;

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
    pub plan: AcceptancePlan,
    pub policy: AcceptancePlanPolicyOutcome,
}

pub async fn plan_acceptance(
    model: &str,
    guidance: &str,
    trace_dir: &Path,
    max_items: usize,
) -> Result<AcceptancePlanningSummary> {
    let gateway = OllamaGateway::new();
    plan_acceptance_with_gateway(&gateway, model, guidance, trace_dir, max_items).await
}

pub async fn plan_acceptance_with_gateway<G: LlmGateway + ?Sized>(
    gateway: &G,
    model: &str,
    guidance: &str,
    trace_dir: &Path,
    max_items: usize,
) -> Result<AcceptancePlanningSummary> {
    let trace = TraceRecorder::create(trace_dir)?;
    let messages = planning_messages(guidance, max_items);
    let response = request_semantic_advisory(
        gateway,
        SemanticAdvisoryRequest {
            advisory_kind: SemanticAdvisoryKind::AcceptancePlanning,
            model,
            messages: &messages,
            response_schema: planning_schema(max_items),
            max_input_chars: DEFAULT_MAX_PLAN_INPUT_CHARS,
            max_output_tokens: DEFAULT_MAX_PLAN_OUTPUT_TOKENS,
            temperature: 0.2,
        },
        &trace,
    )
    .await?;
    let plan: AcceptancePlan = serde_json::from_value(response.raw_proposal)
        .context("decoding acceptance planning proposal")?;
    let policy = evaluate_plan(guidance, &plan, max_items);
    trace.event(
        crate::runtime_events::ACCEPTANCE_PLAN_POLICY_EVALUATED,
        json!({
            "schema_version": ACCEPTANCE_PLAN_SCHEMA_VERSION,
            "plan": &plan,
            "policy": &policy,
            "authority": "measurement_only",
        }),
    )?;

    Ok(AcceptancePlanningSummary {
        model: model.to_string(),
        trace_file: trace.path().to_path_buf(),
        duration_ms: response.duration_ms,
        plan,
        policy,
    })
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
            "You are an isolated acceptance-planning advisor. Decompose public task guidance into a small checklist of externally observable required outcomes. Preserve interactions between requirements when those interactions need separate evidence. Do not solve the task, use tools, invent requirements, or claim anything is complete. Each source_excerpt must be copied verbatim from the task guidance. Return only the requested structured proposal.",
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
    use std::sync::Mutex;

    struct ProposalGateway {
        proposal: Value,
        calls: Mutex<usize>,
    }

    #[async_trait]
    impl LlmGateway for ProposalGateway {
        async fn complete(
            &self,
            _model: &str,
            _messages: &[LlmMessage],
            _tools: Option<&[Box<dyn LlmTool>]>,
            _config: &CompletionConfig,
        ) -> std::result::Result<LlmGatewayResponse, MojenticError> {
            unreachable!("acceptance planning uses structured completion")
        }

        async fn complete_json(
            &self,
            _model: &str,
            messages: &[LlmMessage],
            _schema: Value,
            config: &CompletionConfig,
        ) -> std::result::Result<Value, MojenticError> {
            assert_eq!(config.max_tool_iterations, 0);
            assert!(messages[0].content.as_deref().unwrap().contains("isolated"));
            *self.calls.lock().unwrap() += 1;
            Ok(self.proposal.clone())
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

    #[tokio::test]
    async fn planning_is_measurement_only_and_traceable() {
        let temp = tempfile::tempdir().unwrap();
        let plan = accepted_plan();
        let gateway = ProposalGateway {
            proposal: serde_json::to_value(&plan).unwrap(),
            calls: Mutex::new(0),
        };
        let summary = plan_acceptance_with_gateway(
            &gateway,
            "small-model",
            "Preserve the requested output.",
            temp.path(),
            4,
        )
        .await
        .unwrap();

        assert!(summary.policy.accepted);
        assert_eq!(*gateway.calls.lock().unwrap(), 1);
        let events = std::fs::read_to_string(summary.trace_file).unwrap();
        assert!(events.contains("\"advisory_kind\":\"acceptance_planning\""));
        assert!(events.contains("acceptance_plan.policy_evaluated"));
        assert!(events.contains("\"authority\":\"measurement_only\""));
    }
}
