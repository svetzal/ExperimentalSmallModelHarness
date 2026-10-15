//! What a provider says about one of its models.
//!
//! This module only asks the provider and reads its answer. It holds no
//! harness policy: the fallback table, the explicit window override, the
//! separation between accounting and Ollama's `num_ctx`, and tracing all live
//! in `context_window` and its callers.
//!
//! The query is shaped like a gateway method, keyed by provider and model,
//! because it is expected to move into Mojentic's gateway abstraction. When
//! the pinned mojentic-ru revision provides it, switch to the Mojentic API and
//! delete this module.
use crate::agent::Provider;
use async_trait::async_trait;
use serde_json::{Value, json};

/// A model's context window as its provider advertises it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelContextWindow {
    pub tokens: usize,
    /// The provider field it came from, such as `max_model_len`.
    pub field: String,
}

/// Provider metadata about a model.
#[async_trait]
pub trait ModelMetadata: Send + Sync {
    /// The model's context window, `Ok(None)` when the provider does not
    /// advertise one, or an error when the provider cannot be asked or its
    /// answer is malformed.
    async fn context_window(&self, model: &str) -> Result<Option<ModelContextWindow>, String>;
}

/// The metadata client for `provider`, reading the same host and key
/// environment variables as the provider's gateway.
pub fn for_provider(provider: Provider) -> Box<dyn ModelMetadata> {
    match provider {
        Provider::Omlx => Box::new(OmlxMetadata {
            host: std::env::var("OMLX_HOST").unwrap_or_else(|_| "http://localhost:8000".into()),
            api_key: std::env::var("OMLX_API_KEY")
                .ok()
                .filter(|key| !key.trim().is_empty()),
        }),
        Provider::Ollama => Box::new(OllamaMetadata {
            host: std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://localhost:11434".into()),
        }),
    }
}

struct OmlxMetadata {
    host: String,
    api_key: Option<String>,
}

#[async_trait]
impl ModelMetadata for OmlxMetadata {
    async fn context_window(&self, model: &str) -> Result<Option<ModelContextWindow>, String> {
        let url = format!("{}/v1/models", self.host.trim_end_matches('/'));
        let mut request = reqwest::Client::new().get(url);
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }
        let body = read_json(request).await?;
        omlx_context_window(&body, model)
    }
}

struct OllamaMetadata {
    host: String,
}

#[async_trait]
impl ModelMetadata for OllamaMetadata {
    async fn context_window(&self, model: &str) -> Result<Option<ModelContextWindow>, String> {
        let url = format!("{}/api/show", self.host.trim_end_matches('/'));
        let request = reqwest::Client::new()
            .post(url)
            .json(&json!({ "model": model }));
        let body = read_json(request).await?;
        ollama_context_window(&body)
    }
}

async fn read_json(request: reqwest::RequestBuilder) -> Result<Value, String> {
    let response = request.send().await.map_err(|error| error.to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("HTTP {status}"));
    }
    response.json().await.map_err(|error| error.to_string())
}

/// `max_model_len` of `model` in an oMLX `GET /v1/models` answer.
pub(crate) fn omlx_context_window(
    body: &Value,
    model: &str,
) -> Result<Option<ModelContextWindow>, String> {
    let models = body["data"]
        .as_array()
        .ok_or("the model list has no data array")?;
    let Some(entry) = models.iter().find(|entry| entry["id"] == model) else {
        return Ok(None);
    };
    tokens(&entry["max_model_len"], "max_model_len")
}

/// The context window in an Ollama `POST /api/show` answer: the model's
/// configured `num_ctx` parameter when it has one, because Ollama runs the
/// model with it when a request does not set `num_ctx`, and otherwise the
/// architecture-prefixed `model_info.<arch>.context_length`.
pub(crate) fn ollama_context_window(body: &Value) -> Result<Option<ModelContextWindow>, String> {
    let parameters = body["parameters"].as_str().unwrap_or_default();
    for line in parameters.lines() {
        let mut words = line.split_whitespace();
        if words.next() == Some("num_ctx") {
            let value = words.next().unwrap_or_default();
            return value
                .parse::<usize>()
                .ok()
                .filter(|tokens| *tokens > 0)
                .map(|tokens| {
                    Some(ModelContextWindow {
                        tokens,
                        field: "parameters.num_ctx".into(),
                    })
                })
                .ok_or_else(|| format!("parameters.num_ctx is not a positive number: {value:?}"));
        }
    }
    let Some(info) = body["model_info"].as_object() else {
        return Ok(None);
    };
    match info
        .iter()
        .find(|(key, _)| key.ends_with(".context_length"))
    {
        Some((key, value)) => tokens(value, &format!("model_info.{key}")),
        None => Ok(None),
    }
}

fn tokens(value: &Value, field: &str) -> Result<Option<ModelContextWindow>, String> {
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_u64()
        .filter(|tokens| *tokens > 0)
        .map(|tokens| {
            Some(ModelContextWindow {
                tokens: tokens as usize,
                field: field.into(),
            })
        })
        .ok_or_else(|| format!("{field} is not a positive number: {value}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(tokens: usize, field: &str) -> Option<ModelContextWindow> {
        Some(ModelContextWindow {
            tokens,
            field: field.into(),
        })
    }

    #[test]
    fn omlx_windows_come_from_the_selected_model() {
        let body = json!({"data": [
            {"id": "Qwen3.8-27B-MLX-8bit", "max_model_len": 262_144},
            {"id": "small", "max_model_len": 32_768},
            {"id": "bare"},
        ]});
        assert_eq!(
            omlx_context_window(&body, "small"),
            Ok(window(32_768, "max_model_len"))
        );
        assert_eq!(omlx_context_window(&body, "bare"), Ok(None));
        assert_eq!(omlx_context_window(&body, "missing"), Ok(None));
        assert!(
            omlx_context_window(&json!({"data": [{"id": "m", "max_model_len": "big"}]}), "m")
                .unwrap_err()
                .contains("max_model_len is not a positive number")
        );
        assert!(omlx_context_window(&json!({}), "m").is_err());
    }

    #[test]
    fn ollama_windows_read_the_architecture_prefixed_length() {
        let body = json!({"model_info": {
            "general.architecture": "qwen4_exp",
            "qwen4_exp.context_length": 262_144,
        }});
        assert_eq!(
            ollama_context_window(&body),
            Ok(window(262_144, "model_info.qwen4_exp.context_length"))
        );
        assert_eq!(
            ollama_context_window(&json!({"model_info": {"general.architecture": "x"}})),
            Ok(None)
        );
        assert_eq!(ollama_context_window(&json!({})), Ok(None));
        assert!(
            ollama_context_window(&json!({"model_info": {"llama.context_length": "long"}}))
                .unwrap_err()
                .contains("model_info.llama.context_length is not a positive number")
        );
    }

    #[test]
    fn an_ollama_num_ctx_parameter_wins_because_the_model_runs_with_it() {
        let body = json!({
            "parameters": "temperature 1\nnum_ctx                        32768\nstop \"<|im_end|>\"",
            "model_info": {"qwen4_exp.context_length": 262_144},
        });
        assert_eq!(
            ollama_context_window(&body),
            Ok(window(32_768, "parameters.num_ctx"))
        );
        assert!(
            ollama_context_window(&json!({"parameters": "num_ctx lots"}))
                .unwrap_err()
                .contains("parameters.num_ctx")
        );
    }
}
