//! One stream-json line, reduced to the fields classification reads (OUT-01). The shapes are
//! those of Claude Code 2.1.296's SDK messages.

use assay_app::{RateLimitObserved, RateLimitStatus};
use assay_domain::Aeu;
use serde_json::{Map, Value};

use crate::cost;

/// A stream line that is not a JSON object with a string `type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Unreadable;

/// The parts of one message that matter.
#[derive(Debug, PartialEq)]
pub(super) enum Message {
    /// A `rate_limit_event`; `None` unless its status is `rejected` or `allowed_warning`.
    RateLimit(Option<RateLimitObserved>),
    /// An `assistant` message; `rate_limited` when its `error` is `rate_limit`.
    Assistant { rate_limited: bool },
    /// The `result` message.
    Result(ResultMessage),
    /// Any other message.
    Other,
}

/// The final `result` message.
#[derive(Debug, PartialEq)]
pub(super) struct ResultMessage {
    pub(super) kind: ResultKind,
    /// `total_cost_usd`, when it is a readable cost.
    pub(super) cost: Option<Aeu>,
    /// `structured_output`, when present.
    pub(super) structured: Option<Value>,
    /// `api_error` is `usage_limit_reached`.
    pub(super) usage_limit: bool,
}

/// How a `result` message ended the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ResultKind {
    /// `subtype: success` with `is_error: false`.
    Success,
    /// `subtype: error_max_budget_usd`.
    MaxBudget,
    /// `subtype: error_max_turns`.
    MaxTurns,
    /// Any other ending: an error result, or a subtype this code does not know.
    Error,
}

impl Message {
    pub(super) fn parse(line: &[u8]) -> Result<Self, Unreadable> {
        let Ok(Value::Object(mut object)) = serde_json::from_slice::<Value>(line) else {
            return Err(Unreadable);
        };
        let kind = object
            .get("type")
            .and_then(Value::as_str)
            .ok_or(Unreadable)?;
        Ok(match kind {
            "rate_limit_event" => Self::RateLimit(rate_limit(&object)),
            "assistant" => Self::Assistant {
                rate_limited: object.get("error").and_then(Value::as_str) == Some("rate_limit"),
            },
            "result" => Self::Result(result(&mut object)),
            _ => Self::Other,
        })
    }
}

fn rate_limit(object: &Map<String, Value>) -> Option<RateLimitObserved> {
    let info = object.get("rate_limit_info")?;
    let status = match info.get("status").and_then(Value::as_str)? {
        "rejected" => RateLimitStatus::Rejected,
        "allowed_warning" => RateLimitStatus::AllowedWarning,
        _ => return None,
    };
    Some(RateLimitObserved {
        status,
        resets_at: info.get("resetsAt").and_then(Value::as_u64),
    })
}

fn result(object: &mut Map<String, Value>) -> ResultMessage {
    let is_error = object.get("is_error").and_then(Value::as_bool);
    let kind = match object.get("subtype").and_then(Value::as_str) {
        Some("success") if is_error == Some(false) => ResultKind::Success,
        Some("error_max_budget_usd") => ResultKind::MaxBudget,
        Some("error_max_turns") => ResultKind::MaxTurns,
        _ => ResultKind::Error,
    };
    ResultMessage {
        kind,
        cost: object.get("total_cost_usd").and_then(cost::from_usd),
        structured: object.remove("structured_output"),
        usage_limit: object.get("api_error").and_then(Value::as_str) == Some("usage_limit_reached"),
    }
}

#[cfg(test)]
mod tests {
    use super::{Message, Unreadable};

    #[test]
    fn a_line_that_is_not_an_object_is_unreadable() {
        assert_eq!(Message::parse(b"[1, 2]"), Err(Unreadable));
        assert_eq!(Message::parse(br#"{"no_type": 1}"#), Err(Unreadable));
    }

    #[test]
    fn an_unknown_message_kind_is_skipped() {
        assert_eq!(
            Message::parse(br#"{"type": "stream_event"}"#),
            Ok(Message::Other)
        );
    }
}
