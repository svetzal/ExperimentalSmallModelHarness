//! Isolated semantic advisory effects.
//!
//! Advisory calls may interpret ambiguous information, but they cannot mutate
//! runtime state or invoke tools. Their structured output is always a proposal
//! that an owning deterministic policy must validate before use.

use crate::trace::TraceRecorder;
use anyhow::{Context, Result, bail};
use mojentic::llm::LlmGateway;
use mojentic::llm::gateway::CompletionConfig;
use mojentic::llm::models::LlmMessage;
use serde::Serialize;
use serde_json::{Value, json};
use std::time::Instant;

pub const SEMANTIC_ADVISORY_SCHEMA_VERSION: &str = "semantic_advisory.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SemanticAdvisoryKind {
    InitialContextSelection,
    AcceptancePlanning,
    SituationAnalysis,
    FailureClassification,
}

impl SemanticAdvisoryKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InitialContextSelection => "initial_context_selection",
            Self::AcceptancePlanning => "acceptance_planning",
            Self::SituationAnalysis => "situation_analysis",
            Self::FailureClassification => "failure_classification",
        }
    }
}

pub struct SemanticAdvisoryRequest<'a> {
    pub advisory_kind: SemanticAdvisoryKind,
    pub model: &'a str,
    pub messages: &'a [LlmMessage],
    pub response_schema: Value,
    pub max_input_chars: usize,
    pub max_output_tokens: usize,
    pub temperature: f32,
}

#[derive(Debug, Clone)]
#[must_use = "semantic advisory proposals require deterministic policy evaluation"]
pub struct SemanticAdvisoryResponse {
    pub raw_proposal: Value,
    pub duration_ms: u128,
}

pub async fn request_semantic_advisory<G: LlmGateway + ?Sized>(
    gateway: &G,
    request: SemanticAdvisoryRequest<'_>,
    trace: &TraceRecorder,
) -> Result<SemanticAdvisoryResponse> {
    if request.messages.is_empty() {
        bail!("semantic advisory requires at least one message");
    }
    if request.max_output_tokens == 0 {
        bail!("semantic advisory max_output_tokens must be greater than zero");
    }
    if request.max_input_chars == 0 {
        bail!("semantic advisory max_input_chars must be greater than zero");
    }
    if !request.temperature.is_finite() || request.temperature < 0.0 {
        bail!("semantic advisory temperature must be finite and non-negative");
    }

    let request_chars = request
        .messages
        .iter()
        .map(|message| {
            message
                .content
                .as_deref()
                .unwrap_or_default()
                .chars()
                .count()
        })
        .sum::<usize>();
    if request_chars > request.max_input_chars {
        let error = format!(
            "semantic advisory request is {request_chars} chars, exceeding max_input_chars {}",
            request.max_input_chars
        );
        trace.event(
            crate::runtime_events::SEMANTIC_ADVISORY_REJECTED,
            json!({
                "schema_version": SEMANTIC_ADVISORY_SCHEMA_VERSION,
                "advisory_kind": request.advisory_kind.as_str(),
                "model": request.model,
                "request_chars": request_chars,
                "max_input_chars": request.max_input_chars,
                "reason": "input_budget_exceeded",
                "error": &error,
            }),
        )?;
        bail!(error);
    }
    trace.event(
        crate::runtime_events::SEMANTIC_ADVISORY_REQUESTED,
        json!({
            "schema_version": SEMANTIC_ADVISORY_SCHEMA_VERSION,
            "advisory_kind": request.advisory_kind.as_str(),
            "model": request.model,
            "messages": request.messages,
            "request_chars": request_chars,
            "max_input_chars": request.max_input_chars,
            "response_schema": &request.response_schema,
            "max_output_tokens": request.max_output_tokens,
            "temperature": request.temperature,
            "isolated": true,
            "tools_available": false,
            "authority": "proposal_only",
        }),
    )?;

    let num_predict = i32::try_from(request.max_output_tokens)
        .context("semantic advisory max_output_tokens exceeds i32 range")?;
    let started = Instant::now();
    let result = gateway
        .complete_json(
            request.model,
            request.messages,
            request.response_schema,
            &CompletionConfig {
                temperature: request.temperature,
                max_tokens: request.max_output_tokens,
                num_predict: Some(num_predict),
                max_tool_iterations: 0,
                ..Default::default()
            },
        )
        .await;
    let duration_ms = started.elapsed().as_millis();

    match result {
        Ok(raw_proposal) => {
            trace.event(
                crate::runtime_events::SEMANTIC_ADVISORY_COMPLETED,
                json!({
                    "schema_version": SEMANTIC_ADVISORY_SCHEMA_VERSION,
                    "advisory_kind": request.advisory_kind.as_str(),
                    "model": request.model,
                    "duration_ms": duration_ms,
                    "raw_proposal": &raw_proposal,
                }),
            )?;
            Ok(SemanticAdvisoryResponse {
                raw_proposal,
                duration_ms,
            })
        }
        Err(error) => {
            trace.event(
                crate::runtime_events::SEMANTIC_ADVISORY_FAILED,
                json!({
                    "schema_version": SEMANTIC_ADVISORY_SCHEMA_VERSION,
                    "advisory_kind": request.advisory_kind.as_str(),
                    "model": request.model,
                    "duration_ms": duration_ms,
                    "error": error.to_string(),
                }),
            )?;
            Err(error.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use mojentic::MojenticError;
    use mojentic::llm::gateway::StreamChunk;
    use mojentic::llm::models::LlmGatewayResponse;
    use mojentic::llm::tools::LlmTool;
    use std::sync::Mutex;

    struct ProposalGateway {
        proposal: Value,
        complete_json_calls: Mutex<usize>,
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
            unreachable!("semantic advisories use structured completion")
        }

        async fn complete_json(
            &self,
            _model: &str,
            _messages: &[LlmMessage],
            _schema: Value,
            config: &CompletionConfig,
        ) -> std::result::Result<Value, MojenticError> {
            assert_eq!(config.max_tool_iterations, 0);
            *self.complete_json_calls.lock().unwrap() += 1;
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

    #[tokio::test]
    async fn advisory_is_structured_isolated_and_traceable() {
        let temp = tempfile::tempdir().unwrap();
        let trace = TraceRecorder::create(temp.path()).unwrap();
        let gateway = ProposalGateway {
            proposal: json!({"classification": "relevant"}),
            complete_json_calls: Mutex::new(0),
        };
        let messages = vec![LlmMessage::user("Classify the supplied evidence.")];

        let response = request_semantic_advisory(
            &gateway,
            SemanticAdvisoryRequest {
                advisory_kind: SemanticAdvisoryKind::SituationAnalysis,
                model: "small-model",
                messages: &messages,
                response_schema: json!({"type": "object"}),
                max_input_chars: 1_000,
                max_output_tokens: 64,
                temperature: 0.0,
            },
            &trace,
        )
        .await
        .unwrap();

        assert_eq!(response.raw_proposal["classification"], "relevant");
        assert_eq!(*gateway.complete_json_calls.lock().unwrap(), 1);
        let events = std::fs::read_to_string(trace.path()).unwrap();
        assert!(events.contains("semantic_advisory.requested"));
        assert!(events.contains("\"authority\":\"proposal_only\""));
        assert!(events.contains("\"tools_available\":false"));
    }

    #[tokio::test]
    async fn advisory_rejects_oversized_input_before_calling_provider() {
        let temp = tempfile::tempdir().unwrap();
        let trace = TraceRecorder::create(temp.path()).unwrap();
        let gateway = ProposalGateway {
            proposal: json!({"classification": "relevant"}),
            complete_json_calls: Mutex::new(0),
        };
        let messages = vec![LlmMessage::user("oversized advisory packet")];

        let error = request_semantic_advisory(
            &gateway,
            SemanticAdvisoryRequest {
                advisory_kind: SemanticAdvisoryKind::FailureClassification,
                model: "small-model",
                messages: &messages,
                response_schema: json!({"type": "object"}),
                max_input_chars: 4,
                max_output_tokens: 64,
                temperature: 0.0,
            },
            &trace,
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("max_input_chars"));
        assert_eq!(*gateway.complete_json_calls.lock().unwrap(), 0);
        let events = std::fs::read_to_string(trace.path()).unwrap();
        assert!(events.contains("semantic_advisory.rejected"));
        assert!(!events.contains("semantic_advisory.requested"));
    }
}
