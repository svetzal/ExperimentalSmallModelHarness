//! Time to first token for one provider request.
//!
//! A small measurement around the stream: when the request was sent, when
//! the first token of any kind arrived and what kind it was, and how long the
//! whole request took. Trace writing lives with the caller. This is expected
//! to move into Mojentic's stream metrics; when the pinned mojentic-ru
//! revision provides it, switch to the Mojentic API and delete this module.
use serde::Serialize;
use std::time::{Duration, Instant};

/// What the first streamed token was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenKind {
    Content,
    Thinking,
    ToolCall,
}

/// Timing of one request, as recorded in the trace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RequestTiming {
    /// `None` when the stream produced no token.
    pub time_to_first_token_ms: Option<u64>,
    pub first_token_kind: Option<TokenKind>,
    pub request_duration_ms: u64,
}

#[derive(Debug)]
pub struct FirstTokenClock {
    sent: Instant,
    first: Option<(Duration, TokenKind)>,
}

impl FirstTokenClock {
    /// Start timing as the request is sent.
    pub fn start() -> Self {
        Self::started_at(Instant::now())
    }

    fn started_at(sent: Instant) -> Self {
        Self { sent, first: None }
    }

    /// Note a token. Only the first one counts.
    pub fn observe(&mut self, kind: TokenKind) {
        self.observe_at(kind, Instant::now());
    }

    fn observe_at(&mut self, kind: TokenKind, at: Instant) {
        if self.first.is_none() {
            self.first = Some((at.saturating_duration_since(self.sent), kind));
        }
    }

    /// The timing so far, with the duration up to now.
    pub fn finish(&self) -> RequestTiming {
        self.finish_at(Instant::now())
    }

    fn finish_at(&self, at: Instant) -> RequestTiming {
        RequestTiming {
            time_to_first_token_ms: self.first.map(|(elapsed, _)| elapsed.as_millis() as u64),
            first_token_kind: self.first.map(|(_, kind)| kind),
            request_duration_ms: at.saturating_duration_since(self.sent).as_millis() as u64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_token_of_any_kind_sets_the_time() {
        let sent = Instant::now();
        let ms = Duration::from_millis;
        for (first, kind) in [
            (TokenKind::Content, "content"),
            (TokenKind::Thinking, "thinking"),
            (TokenKind::ToolCall, "tool_call"),
        ] {
            let mut clock = FirstTokenClock::started_at(sent);
            clock.observe_at(first, sent + ms(120));
            clock.observe_at(TokenKind::Content, sent + ms(300));
            let timing = clock.finish_at(sent + ms(900));
            assert_eq!(timing.time_to_first_token_ms, Some(120));
            assert_eq!(timing.first_token_kind, Some(first));
            assert_eq!(timing.request_duration_ms, 900);
            assert_eq!(serde_json::to_value(first).unwrap(), kind);
        }
    }

    #[test]
    fn a_stream_without_tokens_has_no_first_token_time() {
        let sent = Instant::now();
        let timing = FirstTokenClock::started_at(sent).finish_at(sent + Duration::from_millis(40));
        assert_eq!(
            timing,
            RequestTiming {
                time_to_first_token_ms: None,
                first_token_kind: None,
                request_duration_ms: 40,
            }
        );
    }
}
