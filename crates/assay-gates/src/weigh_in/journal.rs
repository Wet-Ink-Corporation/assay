//! One Weigh-In journal, `.kb/_ledger/journal/<session>.jsonl`: one JSON event per line
//! (decision 0003). Only the fields the sets need are read: every event's `type`, `item_id`
//! and `ts`, `item_raised`'s `blocks` and `item_triaged`'s `door` and `authority`. The rest of
//! each event is Weigh-In's and is not modelled. A journal that is not exactly this is refused
//! whole (wi-b5affa): a blank line, a final line without its newline (the hook deletes a
//! cut-short line before committing), a line that is not one JSON object or repeats a key, an
//! unknown `type`, an id that does not parse or a `ts` that is not `YYYY-MM-DDTHH:MM:SSZ`.

use std::num::NonZeroU64;

use assay_app::parse_json_strict;
use serde_json::{Map, Value};

use super::id::ItemId;

/// What an `item_raised` event says about what the item blocks (04 §9: missing is unknown,
/// never none).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Blocks {
    /// `blocks: null`.
    Nothing,
    /// `blocks` is set; its free text is the agent's and is not reported.
    Set,
    /// No `blocks` field.
    Unknown,
}

/// The ledger status an event moves its item to (04 §9: `Status::is_open`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Status {
    Raised,
    Triaged,
    Grounded,
    Presented,
    Deferred,
    Decided,
    Defaulted,
    Dismissed,
}

impl Status {
    /// Raised, triaged, grounded, presented and deferred items are open.
    pub(super) const fn is_open(self) -> bool {
        match self {
            Self::Raised | Self::Triaged | Self::Grounded | Self::Presented | Self::Deferred => {
                true
            }
            Self::Decided | Self::Defaulted | Self::Dismissed => false,
        }
    }

    /// The status as the findings name it.
    pub(super) const fn word(self) -> &'static str {
        match self {
            Self::Raised => "raised",
            Self::Triaged => "triaged",
            Self::Grounded => "grounded",
            Self::Presented => "presented",
            Self::Deferred => "deferred",
            Self::Decided => "decided",
            Self::Defaulted => "defaulted",
            Self::Dismissed => "dismissed",
        }
    }
}

/// What one event records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Change {
    /// `item_raised`.
    Raised(Blocks),
    /// `item_triaged`, with its triaged `door` and `authority` (`None` when null or absent).
    Triaged {
        door: Option<String>,
        authority: Option<String>,
    },
    /// Any other status event.
    Moved(Status),
    /// `record_written`: no status change.
    Recorded,
}

impl Change {
    /// The status the event moves its item to, if any.
    pub(super) const fn status(&self) -> Option<Status> {
        match self {
            Self::Raised(_) => Some(Status::Raised),
            Self::Triaged { .. } => Some(Status::Triaged),
            Self::Moved(status) => Some(*status),
            Self::Recorded => None,
        }
    }
}

/// One journal line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Event {
    pub(super) item: ItemId,
    /// `YYYY-MM-DDTHH:MM:SSZ`, so text order is time order.
    pub(super) ts: String,
    pub(super) line: NonZeroU64,
    pub(super) change: Change,
}

/// Why a journal was refused, in the adapter's words (never quoting the journal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Refusal {
    /// The line at fault; `None` for the missing final newline.
    pub(super) line: Option<NonZeroU64>,
    pub(super) fault: JournalFault,
}

/// What was wrong with a journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum JournalFault {
    NoFinalNewline,
    BlankLine,
    NotAnObject,
    NotAnId,
    NotATimestamp,
    NoType,
    UnknownType,
    Blocks,
    Triage,
    TooManyLines,
}

impl JournalFault {
    /// What is wrong, completing "the journal ...".
    pub(super) const fn text(self) -> &'static str {
        match self {
            Self::NoFinalNewline => "ends in a line without its newline",
            Self::BlankLine => "has a blank line",
            Self::NotAnObject => "has a line that is not one JSON object with distinct keys",
            Self::NotAnId => "has an `item_id` that is not a Weigh-In id",
            Self::NotATimestamp => "has a `ts` that is not `YYYY-MM-DDTHH:MM:SSZ`",
            Self::NoType => "has an event without a `type`",
            Self::UnknownType => "has an event `type` this adapter does not know",
            Self::Blocks => "has a `blocks` that is neither null nor text",
            Self::Triage => "has a triaged `door` or `authority` that is neither null nor text",
            Self::TooManyLines => "has too many lines",
        }
    }
}

/// The shape of `ts`: `d` is an ASCII digit, anything else itself.
const TS_SHAPE: &[u8; 20] = b"dddd-dd-ddTdd:dd:ddZ";

/// The events of the journal `bytes`, in line order.
pub(super) fn parse(bytes: &[u8]) -> Result<Vec<Event>, Refusal> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let Some(body) = bytes.strip_suffix(b"\n") else {
        return Err(Refusal {
            line: None,
            fault: JournalFault::NoFinalNewline,
        });
    };
    let mut line = NonZeroU64::MIN;
    let mut events = Vec::new();
    for text in body.split(|byte| *byte == b'\n') {
        let at = |fault| Refusal {
            line: Some(line),
            fault,
        };
        events.push(event(text, line).map_err(at)?);
        line = line
            .checked_add(1)
            .ok_or_else(|| at(JournalFault::TooManyLines))?;
    }
    Ok(events)
}

/// The event on journal line `line`.
fn event(text: &[u8], line: NonZeroU64) -> Result<Event, JournalFault> {
    if text.is_empty() {
        return Err(JournalFault::BlankLine);
    }
    let value = parse_json_strict(text).map_err(|_json| JournalFault::NotAnObject)?;
    let Value::Object(mut object) = value else {
        return Err(JournalFault::NotAnObject);
    };
    let item = take_text(&mut object, "item_id")
        .and_then(ItemId::new)
        .ok_or(JournalFault::NotAnId)?;
    let ts = take_text(&mut object, "ts")
        .filter(|ts| timestamp(ts))
        .ok_or(JournalFault::NotATimestamp)?;
    let kind = take_text(&mut object, "type").ok_or(JournalFault::NoType)?;
    Ok(Event {
        item,
        ts,
        line,
        change: change(&kind, &mut object)?,
    })
}

/// The text field `key`, taken out of `object`, when it is text.
fn take_text(object: &mut Map<String, Value>, key: &str) -> Option<String> {
    match object.remove(key) {
        Some(Value::String(text)) => Some(text),
        _ => None,
    }
}

/// What an event of type `kind` with fields `object` records.
fn change(kind: &str, object: &mut Map<String, Value>) -> Result<Change, JournalFault> {
    Ok(match kind {
        "item_raised" => Change::Raised(match object.get("blocks") {
            None => Blocks::Unknown,
            Some(Value::Null) => Blocks::Nothing,
            Some(Value::String(_)) => Blocks::Set,
            Some(_) => return Err(JournalFault::Blocks),
        }),
        "item_triaged" => Change::Triaged {
            door: nullable(object, "door")?,
            authority: nullable(object, "authority")?,
        },
        "item_grounded" => Change::Moved(Status::Grounded),
        "item_presented" => Change::Moved(Status::Presented),
        "item_deferred" => Change::Moved(Status::Deferred),
        "item_decided" => Change::Moved(Status::Decided),
        "item_defaulted" => Change::Moved(Status::Defaulted),
        "item_dismissed" => Change::Moved(Status::Dismissed),
        "record_written" => Change::Recorded,
        _ => return Err(JournalFault::UnknownType),
    })
}

/// The text field `key`, taken out of `object`; `None` when it is null or absent (not triaged).
fn nullable(object: &mut Map<String, Value>, key: &str) -> Result<Option<String>, JournalFault> {
    match object.remove(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(JournalFault::Triage),
    }
}

/// Whether `ts` has exactly the shape `YYYY-MM-DDTHH:MM:SSZ`.
fn timestamp(ts: &str) -> bool {
    ts.len() == TS_SHAPE.len()
        && ts.bytes().zip(TS_SHAPE).all(|(byte, shape)| match shape {
            b'd' => byte.is_ascii_digit(),
            _ => byte == *shape,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timestamp_has_exactly_the_utc_seconds_shape() {
        assert!(timestamp("2026-10-11T06:11:26Z"));
        for refused in [
            "2026-10-11T06:11:26",
            "2026-10-11T06:11:26.5Z",
            "2026-10-11 06:11:26Z",
            "2026-1a-11T06:11:26Z",
            "2026-10-11T06:11:26+00:00",
            "",
        ] {
            assert!(!timestamp(refused), "{refused}");
        }
    }

    #[test]
    fn an_empty_journal_has_no_events() {
        assert_eq!(parse(b""), Ok(Vec::new()));
    }
}
