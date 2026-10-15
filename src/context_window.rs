//! The context window Microgentic accounts against, and where it came from.
//!
//! The accounting window feeds measurement: the ledger's known-window
//! utilization and pressure band, the guidance budget, the context panel and
//! the status gauge. It never sets Ollama's `num_ctx`: that stays the explicit
//! `--context-window-tokens` value or the model table entry, exactly as
//! before, so a discovered window cannot make Ollama allocate a larger cache.
//! Run policies that read the window (compaction, file observation budgets,
//! stall projections, conversation trimming) also keep the configured value.
use crate::model_metadata::ModelMetadata;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// How long discovery may take before the run falls back.
pub const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(2);

/// Where the accounting window came from, in order of precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowSource {
    /// `--context-window-tokens`.
    Explicit,
    /// The provider's model metadata.
    Provider,
    /// Microgentic's table of known models.
    FallbackTable,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextWindow {
    pub tokens: Option<usize>,
    pub source: WindowSource,
    /// The provider field, when the provider gave the window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Why discovery did not give the window, when it was tried.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery_error: Option<String>,
    /// How long discovery took, when it was tried.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery_ms: Option<u64>,
    /// Reused from an earlier exchange of the same conversation.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cached: bool,
    /// The larger window the source gave, when the `num_ctx` that Ollama
    /// receives is smaller and so sets the real limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advertised_tokens: Option<usize>,
}

impl ContextWindow {
    /// The window known without asking the provider.
    pub fn configured(explicit: Option<usize>, table: Option<usize>) -> Self {
        let (tokens, source) = match (explicit, table) {
            (Some(tokens), _) => (Some(tokens), WindowSource::Explicit),
            (None, Some(tokens)) => (Some(tokens), WindowSource::FallbackTable),
            (None, None) => (None, WindowSource::Unknown),
        };
        Self {
            tokens,
            source,
            field: None,
            discovery_error: None,
            discovery_ms: None,
            cached: false,
            advertised_tokens: None,
        }
    }

    /// Ollama runs a request with the `num_ctx` it sends, so a larger
    /// advertised window is not usable. Keep the advertised size for the
    /// record and account against `num_ctx`.
    pub fn within_num_ctx(self, num_ctx: usize) -> Self {
        match self.tokens {
            Some(tokens) if num_ctx > 0 && tokens > num_ctx => Self {
                tokens: Some(num_ctx),
                advertised_tokens: Some(tokens),
                ..self
            },
            _ => self,
        }
    }

    /// Ask the provider unless an explicit window was given. A provider
    /// answer beats the table; a failure keeps the configured window and
    /// records why.
    pub async fn discover(self, metadata: &dyn ModelMetadata, model: &str) -> Self {
        self.discover_within(metadata, model, DISCOVERY_TIMEOUT)
            .await
    }

    async fn discover_within(
        self,
        metadata: &dyn ModelMetadata,
        model: &str,
        limit: Duration,
    ) -> Self {
        if self.source == WindowSource::Explicit {
            return self;
        }
        let started = Instant::now();
        let answer = tokio::time::timeout(limit, metadata.context_window(model)).await;
        let discovery_ms = Some(started.elapsed().as_millis() as u64);
        match answer {
            Ok(Ok(Some(found))) => Self {
                tokens: Some(found.tokens),
                source: WindowSource::Provider,
                field: Some(found.field),
                discovery_error: None,
                discovery_ms,
                cached: false,
                advertised_tokens: None,
            },
            Ok(Ok(None)) => Self {
                discovery_error: Some(
                    "the provider does not advertise a window for this model".into(),
                ),
                discovery_ms,
                ..self
            },
            Ok(Err(error)) => Self {
                discovery_error: Some(error),
                discovery_ms,
                ..self
            },
            Err(_) => Self {
                discovery_error: Some(format!("no answer within {} ms", limit.as_millis())),
                discovery_ms,
                ..self
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_metadata::ModelContextWindow;
    use async_trait::async_trait;

    struct Fixed(Result<Option<ModelContextWindow>, String>);

    #[async_trait]
    impl ModelMetadata for Fixed {
        async fn context_window(&self, _: &str) -> Result<Option<ModelContextWindow>, String> {
            self.0.clone()
        }
    }

    struct Silent;

    #[async_trait]
    impl ModelMetadata for Silent {
        async fn context_window(&self, _: &str) -> Result<Option<ModelContextWindow>, String> {
            std::future::pending().await
        }
    }

    fn provider(tokens: usize) -> Fixed {
        Fixed(Ok(Some(ModelContextWindow {
            tokens,
            field: "max_model_len".into(),
        })))
    }

    #[tokio::test]
    async fn the_fallback_order_is_explicit_then_provider_then_table() {
        let explicit = ContextWindow::configured(Some(8_192), Some(262_144))
            .discover(&provider(131_072), "m")
            .await;
        assert_eq!(
            (explicit.tokens, explicit.source),
            (Some(8_192), WindowSource::Explicit)
        );
        assert_eq!(
            explicit.discovery_ms, None,
            "an explicit window skips discovery"
        );

        let discovered = ContextWindow::configured(None, Some(262_144))
            .discover(&provider(131_072), "m")
            .await;
        assert_eq!(
            (
                discovered.tokens,
                discovered.source,
                discovered.field.as_deref()
            ),
            (Some(131_072), WindowSource::Provider, Some("max_model_len"))
        );

        let table = ContextWindow::configured(None, Some(262_144))
            .discover(&Fixed(Ok(None)), "m")
            .await;
        assert_eq!(
            (table.tokens, table.source),
            (Some(262_144), WindowSource::FallbackTable)
        );
        assert!(
            table
                .discovery_error
                .unwrap()
                .contains("does not advertise")
        );

        let unknown = ContextWindow::configured(None, None)
            .discover(&Fixed(Err("HTTP 404".into())), "m")
            .await;
        assert_eq!(
            (unknown.tokens, unknown.source),
            (None, WindowSource::Unknown)
        );
        assert_eq!(unknown.discovery_error.as_deref(), Some("HTTP 404"));
    }

    #[test]
    fn ollama_accounts_against_the_num_ctx_it_sends() {
        let advertised = ContextWindow {
            tokens: Some(262_144),
            source: WindowSource::Provider,
            ..ContextWindow::configured(None, None)
        };
        let capped = advertised.clone().within_num_ctx(32_768);
        assert_eq!(capped.tokens, Some(32_768));
        assert_eq!(capped.advertised_tokens, Some(262_144));
        assert_eq!(capped.source, WindowSource::Provider);
        assert_eq!(advertised.clone().within_num_ctx(524_288), advertised);
        let unknown = ContextWindow::configured(None, None);
        assert_eq!(unknown.clone().within_num_ctx(32_768), unknown);
    }

    #[tokio::test]
    async fn a_silent_provider_times_out_and_falls_back() {
        let window = ContextWindow::configured(None, Some(262_144))
            .discover_within(&Silent, "m", Duration::from_millis(20))
            .await;
        assert_eq!(window.source, WindowSource::FallbackTable);
        assert_eq!(
            window.discovery_error.as_deref(),
            Some("no answer within 20 ms")
        );
    }
}
