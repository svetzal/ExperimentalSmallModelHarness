//! Context residency recording for everyday runs and the conversation.
//!
//! This is measurement only: it records what each request carries and why,
//! and never changes what is sent. Benchmark runs have no residency recorder,
//! so their traces are unchanged.
use crate::context_window::ContextWindow;
use serde_json::{Value, json};

/// What everyday runs and the conversation record about their context.
#[derive(Debug)]
pub(crate) struct Residency {
    window: ContextWindow,
}

impl Residency {
    pub(crate) fn new(window: ContextWindow) -> Self {
        Self { window }
    }

    /// Add the known window's accounting to a context ledger. The ledger's
    /// own window, utilization and pressure band stay on the configured
    /// window, because run policies read them.
    pub(crate) fn annotate_ledger(&self, ledger: &mut Value) {
        let estimated = ledger["estimated_tokens"].as_u64().unwrap_or(0) as usize;
        let utilization = self
            .window
            .tokens
            .filter(|tokens| *tokens > 0)
            .map(|tokens| estimated as f64 / tokens as f64);
        ledger["known_window"] = json!({
            "tokens": self.window.tokens,
            "source": self.window.source,
            "utilization": utilization,
            "pressure_band": crate::agent::pressure_band(utilization),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_window::WindowSource;

    #[test]
    fn ledgers_gain_the_known_window_beside_the_configured_one() {
        let residency = Residency::new(ContextWindow {
            tokens: Some(1_000),
            source: WindowSource::Provider,
            field: Some("max_model_len".into()),
            discovery_error: None,
            discovery_ms: Some(3),
            cached: false,
        });
        let mut ledger = json!({"estimated_tokens": 200, "context_window_tokens": null, "pressure_band": "unknown"});
        residency.annotate_ledger(&mut ledger);
        assert_eq!(
            ledger["pressure_band"], "unknown",
            "the policy band is unchanged"
        );
        assert_eq!(
            ledger["known_window"],
            json!({"tokens": 1_000, "source": "provider", "utilization": 0.2, "pressure_band": "yellow"})
        );
    }
}
