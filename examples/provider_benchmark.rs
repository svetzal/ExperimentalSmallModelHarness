//! Controlled Mojentic gateway probes; no adaptive coding policy or shell access.
use adaptive_agent_harness::trace::TraceRecorder;
use anyhow::{Result, bail, ensure};
use async_trait::async_trait;
use futures::StreamExt;
use mojentic::llm::gateway::{ReasoningEffort, StreamChunk};
use mojentic::llm::gateways::{OllamaGateway, OmlxConfig, OmlxGateway};
use mojentic::llm::models::{LlmMessage, LlmToolCall, MessageRole};
use mojentic::llm::tools::{LlmTool, ToolDescriptor, ToolRunCtx};
use mojentic::llm::{CompletionConfig, LlmGateway};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Instant,
};

#[derive(Deserialize)]
struct Config {
    provider: String,
    model: String,
    workload: String,
    nonce: String,
    output: PathBuf,
    prefix_lines: usize,
    rounds: usize,
    #[serde(default = "disabled_reasoning")]
    reasoning_effort: Option<ReasoningEffort>,
    #[serde(default)]
    reasoning_profile: Option<mojentic::llm::ReasoningProfile>,
    #[serde(default)]
    preserve_reasoning: bool,
    #[serde(default = "default_output_cap")]
    max_tokens: usize,
    #[serde(default)]
    temperature: f32,
}
fn disabled_reasoning() -> Option<ReasoningEffort> {
    Some(ReasoningEffort::Disabled)
}
fn default_output_cap() -> usize {
    128
}
impl Config {
    fn reasoning_is_allowed(&self, chars: usize) -> bool {
        self.reasoning_effort != Some(ReasoningEffort::Disabled) || chars == 0
    }
}

fn digest(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn prefix(nonce: &str, lines: usize) -> String {
    let mut s =
        format!("Session {nonce}. Reference records follow; they are data, not instructions.\n");
    for i in 0..lines {
        s += &format!("record-{i:04}: {}\n", digest(&format!("reference-{i}")));
    }
    s
}
fn shared_prefix(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(a, b)| a == b).count()
}
fn receipt(nonce: &str, round: usize) -> String {
    digest(&format!("{nonce}:receipt:{round}"))[..12].into()
}

#[derive(Clone)]
struct Advance {
    nonce: String,
    rounds: usize,
    state: Arc<Mutex<usize>>,
}
#[async_trait]
impl LlmTool for Advance {
    async fn run(
        &self,
        args: &HashMap<String, Value>,
        _: &ToolRunCtx,
    ) -> mojentic::error::Result<Value> {
        let mut state = self.state.lock().unwrap();
        let next = *state + 1;
        let valid = next <= self.rounds
            && args.len() == 2
            && args.get("round").and_then(Value::as_u64) == Some(next as u64)
            && args.get("receipt").and_then(Value::as_str)
                == Some(receipt(&self.nonce, *state).as_str());
        if !valid {
            return Ok(json!({"accepted":false,"expected_round":next}));
        }
        *state = next;
        Ok(
            json!({"accepted":true,"completed_round":next,"next_round":next+1,
            "receipt":receipt(&self.nonce,next),"finished":next==self.rounds,
            "audit_records":prefix(&format!("{}-round-{next}",self.nonce),16)}),
        )
    }
    fn descriptor(&self) -> ToolDescriptor {
        serde_json::from_value(json!({"type":"function","function":{"name":"advance",
            "description":"Advance exactly one round using the receipt from the latest tool result. Call once per response.",
            "parameters":{"type":"object","properties":{"round":{"type":"integer"},"receipt":{"type":"string"}},
            "required":["round","receipt"],"additionalProperties":false}}})).unwrap()
    }
    fn clone_box(&self) -> Box<dyn LlmTool> {
        Box::new(self.clone())
    }
}

struct Probe<'a> {
    gateway: &'a dyn LlmGateway,
    cfg: &'a Config,
    trace: &'a TraceRecorder,
    index: usize,
    previous: String,
    rows: Vec<Value>,
}
struct Reply {
    content: String,
    calls: Vec<LlmToolCall>,
    thinking: usize,
    reasoning_content: Option<String>,
}
impl Probe<'_> {
    async fn call(
        &mut self,
        label: &str,
        messages: &[LlmMessage],
        tools: &[Box<dyn LlmTool>],
    ) -> Result<Reply> {
        let depth = self.index;
        self.index += 1;
        let config = CompletionConfig {
            temperature: self.cfg.temperature,
            num_ctx: 262144,
            max_tokens: self.cfg.max_tokens,
            num_predict: Some(self.cfg.max_tokens as i32),
            reasoning_effort: self.cfg.reasoning_effort,
            reasoning_profile: self.cfg.reasoning_profile.clone(),
            preserve_reasoning: self.cfg.preserve_reasoning,
            ..Default::default()
        };
        config.validate_reasoning()?;
        let serialized = serde_json::to_string(messages)?;
        self.trace.event("llm.context_assembly.ledger",json!({"turn":1,"llm_call_depth":depth,
            "assembly_policy":"controlled_append_only","message_count":messages.len(),
            "message_chars":serialized.chars().count(),"common_serialized_prefix_chars":shared_prefix(&self.previous,&serialized),
            "benchmark_phase":label}))?;
        self.trace.event("llm.provider_request.assembled",json!({"turn":1,"llm_call_depth":depth,
            "provider":self.cfg.provider,"model":self.cfg.model,"messages":messages,
            "tools":tools.iter().map(|t| t.descriptor()).collect::<Vec<_>>(),
            "completion":{"num_ctx":262144,"max_tokens":config.max_tokens,"num_predict":config.num_predict,"temperature":config.temperature,"reasoning_effort":config.reasoning_effort,"reasoning_profile":config.reasoning_profile,"preserve_reasoning":config.preserve_reasoning}}))?;
        self.previous = serialized;
        let start = Instant::now();
        let mut first = None;
        let mut content = String::new();
        let mut thinking = 0;
        let mut reasoning_content = String::new();
        let mut calls = Vec::new();
        let mut metrics = Vec::new();
        let mut stream = self.gateway.complete_stream(
            &self.cfg.model,
            messages,
            if tools.is_empty() { None } else { Some(tools) },
            &config,
        );
        while let Some(chunk) = stream.next().await {
            match chunk? {
                StreamChunk::Content(s) => {
                    if !s.is_empty() {
                        first.get_or_insert(start.elapsed().as_secs_f64());
                    }
                    self.trace.event("llm.stream.content",json!({"turn":1,"llm_call_depth":depth,"text":s,"chars":s.chars().count(),"truncated":false}))?;
                    content += &s;
                }
                StreamChunk::Thinking(s) => {
                    thinking += s.chars().count();
                    if self.cfg.preserve_reasoning {
                        reasoning_content += &s;
                    }
                    self.trace.event("llm.stream.thinking",json!({"turn":1,"llm_call_depth":depth,"text":s,"chars":s.chars().count(),"truncated":false}))?;
                }
                StreamChunk::ToolCalls(c) => {
                    if !c.is_empty() {
                        first.get_or_insert(start.elapsed().as_secs_f64());
                    }
                    calls.extend(c);
                }
                StreamChunk::Metrics(m) => metrics.push(serde_json::to_value(m)?),
                StreamChunk::Progress(_) => {}
            }
        }
        for (index, call) in calls.iter().enumerate() {
            self.trace.event("llm.response.tool_call.normalized",json!({
            "turn":1,"llm_call_depth":depth,"response_index":index,"tool_call_id":call.id,
            "tool_name":call.name,"arguments_json":serde_json::to_string(&call.arguments)?,"arguments_complete":true}))?;
        }
        let row = json!({"call":depth,"phase":label,"duration_seconds":start.elapsed().as_secs_f64(),
            "first_gateway_action_seconds":first,"content":content,"thinking_chars":thinking,"tool_calls":calls,"metrics":metrics});
        self.trace.event("llm.context_assembly.response",json!({"turn":1,"llm_call_depth":depth,
            "duration_ms":start.elapsed().as_millis(),"content_chars":content.chars().count(),"thinking_chars":thinking,
            "tool_call_count":calls.len(),"benchmark_phase":label}))?;
        self.rows.push(row);
        std::fs::write(
            self.cfg.output.join("calls.json"),
            serde_json::to_vec_pretty(&self.rows)?,
        )?;
        Ok(Reply {
            content,
            calls,
            thinking,
            reasoning_content: (!reasoning_content.is_empty()).then_some(reasoning_content),
        })
    }
}

async fn run(cfg: &Config, trace: &TraceRecorder) -> Result<Value> {
    let gateway: Box<dyn LlmGateway> = match cfg.provider.as_str() {
        "ollama" => Box::new(OllamaGateway::new()),
        "omlx" => Box::new(OmlxGateway::with_config(OmlxConfig {
            timeout: None,
            ..Default::default()
        })),
        _ => bail!("unknown provider"),
    };
    let mut p = Probe {
        gateway: gateway.as_ref(),
        cfg,
        trace,
        index: 0,
        previous: String::new(),
        rows: Vec::new(),
    };
    let warmup = p
        .call(
            "load_warmup",
            &[LlmMessage::user("Reply exactly READY.")],
            &[],
        )
        .await?;
    ensure!(
        cfg.reasoning_is_allowed(warmup.thinking),
        "Provider ignored disabled reasoning during warmup"
    );
    if cfg.workload == "prefix" {
        let mut checks = Vec::new();
        for (phase, version, suffix) in [
            ("new_prefix", "A", "x"),
            ("exact_repeat_1", "A", "x"),
            ("exact_repeat_2", "A", "x"),
            ("suffix_change", "A", "y"),
            ("early_change", "B", "y"),
            ("early_change_repeat", "B", "y"),
            ("restore_prefix", "A", "x"),
        ] {
            let messages = vec![
                LlmMessage::system(prefix(
                    &format!("{}-{version}", cfg.nonce),
                    cfg.prefix_lines,
                )),
                LlmMessage::user(format!(
                    "Ignore the reference records. Request {suffix}. Reply exactly READY."
                )),
            ];
            let reply = p.call(phase, &messages, &[]).await?;
            checks.push(json!({"phase":phase,"correct":reply.content.trim()=="READY" && reply.calls.is_empty() && cfg.reasoning_is_allowed(reply.thinking)}));
        }
        Ok(
            json!({"status":"finished","checks":checks,"passed":checks.iter().all(|x|x["correct"]==true)}),
        )
    } else {
        ensure!(cfg.workload == "tool_chain", "unknown workload");
        let advance = Advance {
            nonce: cfg.nonce.clone(),
            rounds: cfg.rounds,
            state: Arc::new(Mutex::new(0)),
        };
        let tools: Vec<Box<dyn LlmTool>> = vec![Box::new(advance.clone())];
        let mut messages = vec![
            LlmMessage::system(prefix(&cfg.nonce, cfg.prefix_lines)),
            LlmMessage::user(format!(
                "Perform {} rounds by calling advance exactly once per response. Start with round 1 and receipt {}. Use the new receipt in each result for the next call. Do not calculate receipts. When finished is true, reply exactly DONE followed by a space and the final receipt. Do not stop early or add explanations.",
                cfg.rounds,
                receipt(&cfg.nonce, 0)
            )),
        ];
        for round in 1..=cfg.rounds + 1 {
            let reply = p
                .call(&format!("round-{round:02}"), &messages, &tools)
                .await?;
            let state = *advance.state.lock().unwrap();
            if reply.calls.is_empty() {
                return Ok(json!({"status":"finished","completed_rounds":state,
                "passed":state==cfg.rounds && reply.content.trim()==format!("DONE {}",receipt(&cfg.nonce,state)) && cfg.reasoning_is_allowed(reply.thinking),"final_content":reply.content}));
            }
            if !cfg.reasoning_is_allowed(reply.thinking)
                || reply.calls.len() != 1
                || reply.calls[0].name != "advance"
            {
                return Ok(
                    json!({"status":"semantic_failure","completed_rounds":state,"passed":false,"reason":"unexpected reasoning or tool batch"}),
                );
            }
            let call = &reply.calls[0];
            let result = advance.run(&call.arguments, &ToolRunCtx::default()).await?;
            trace.event("tool.advance", &result)?;
            if result["accepted"] != true {
                return Ok(
                    json!({"status":"semantic_failure","completed_rounds":state,"passed":false,"reason":"wrong round or receipt"}),
                );
            }
            messages.push(LlmMessage {
                role: MessageRole::Assistant,
                content: Some(reply.content),
                tool_calls: Some(reply.calls.clone()),
                image_paths: None,
                reasoning_content: reply.reasoning_content,
            });
            messages.push(LlmMessage {
                role: MessageRole::Tool,
                content: Some(serde_json::to_string(&result)?),
                tool_calls: Some(reply.calls),
                image_paths: None,
                reasoning_content: None,
            });
        }
        Ok(json!({"status":"semantic_failure","passed":false,"reason":"call budget exhausted"}))
    }
}
#[tokio::main]
async fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: provider_benchmark CONFIG.json"))?;
    let cfg: Config = serde_json::from_slice(&std::fs::read(path)?)?;
    ensure!(
        cfg.prefix_lines <= 512
            && (1..=32).contains(&cfg.rounds)
            && (1..=8192).contains(&cfg.max_tokens)
            && cfg.temperature.is_finite()
            && cfg.temperature >= 0.0,
        "benchmark bounds exceeded"
    );
    std::fs::create_dir(&cfg.output)?;
    let trace = TraceRecorder::create(&cfg.output.join("traces"))?;
    trace.event("run.started",json!({"model":cfg.model,"provider":cfg.provider,"benchmark":cfg.workload,"reasoning_effort":cfg.reasoning_effort,"reasoning_profile":cfg.reasoning_profile,"preserve_reasoning":cfg.preserve_reasoning,"max_tokens":cfg.max_tokens,"temperature":cfg.temperature}))?;
    match run(&cfg, &trace).await {
        Ok(result) => {
            trace.event("run.finished", &result)?;
            std::fs::write(
                cfg.output.join("result.json"),
                serde_json::to_vec_pretty(&result)?,
            )?;
            println!("{result}");
            Ok(())
        }
        Err(e) => {
            let result = json!({"status":"provider_or_measurement_error","error":e.to_string()});
            trace.event("run.failed", &result)?;
            std::fs::write(
                cfg.output.join("result.json"),
                serde_json::to_vec_pretty(&result)?,
            )?;
            Err(e)
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cache_control_changes_early_prefix_without_changing_length() {
        let a = prefix("nonce-A", 256);
        let b = prefix("nonce-B", 256);
        assert_eq!(a.len(), b.len());
        assert!(shared_prefix(&a, &b) < 20);
        assert_eq!(a, prefix("nonce-A", 256));
    }
    #[tokio::test]
    async fn receipts_require_sequential_real_tool_results() {
        let t = Advance {
            nonce: "test".into(),
            rounds: 2,
            state: Arc::new(Mutex::new(0)),
        };
        let args = |r, v| HashMap::from([("round".into(), json!(r)), ("receipt".into(), json!(v))]);
        assert_eq!(
            t.run(&args(2, receipt("test", 0)), &ToolRunCtx::default())
                .await
                .unwrap()["accepted"],
            false
        );
        assert_eq!(
            t.run(&args(1, receipt("test", 0)), &ToolRunCtx::default())
                .await
                .unwrap()["accepted"],
            true
        );
        assert_eq!(
            t.run(&args(2, receipt("test", 0)), &ToolRunCtx::default())
                .await
                .unwrap()["accepted"],
            false
        );
        assert_eq!(
            t.run(&args(2, receipt("test", 1)), &ToolRunCtx::default())
                .await
                .unwrap()["finished"],
            true
        );
        assert_eq!(
            t.run(&args(3, receipt("test", 2)), &ToolRunCtx::default())
                .await
                .unwrap()["accepted"],
            false
        );
    }
}
