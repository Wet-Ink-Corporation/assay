//! The adapter's warnings over a `weighin.ci-check/1` report. None decides anything: the gate is
//! not required (04 §9), and `decide` reads only required gates (rubric rule 1). Ids are
//! validated hex and statuses and triaged values are closed sets; a journal path goes through
//! [`plain_name`] into a code span. Questions and deadlines are never read into a finding.

use std::num::NonZeroU64;

use assay_app::{clip_chars, plain_name};
use assay_domain::{
    Authority, BlockingCause, BlockingCauses, Class, Door, Finding, FindingKind, Severity, Side,
    Standing, standing,
};

use super::report::{Blocks, OpenItem, Reason, Report, Violation};

/// Most item and violation findings in one document; one more says how many were left out.
const MAX_FINDINGS: usize = 1_000;

/// Where item findings sit: the journal directory, since the report does not say which journal
/// or line raised an item.
const JOURNALS: &str = ".kb/_ledger/journal";

/// Rule of an open, triaged item that blocks (04 §9 rule 1).
const OPEN_BLOCKING: &str = "weigh-in-open-blocking";
/// Rule of an open item not triaged yet (the reason 04 §9 names).
const PENDING_TRIAGE: &str = "weigh-in-pending-triage";
/// Rule of a journal that breaks the append-only rule (04 §9 rule 5).
const JOURNAL_REWRITTEN: &str = "weigh-in-journal-rewritten";
/// Rule of the note that findings were left out.
const TRUNCATED: &str = "weigh-in-truncated";

/// Most characters of `evidence`, `title` and `body` (03 §3).
const EVIDENCE_CHARS: usize = 400;
const TITLE_CHARS: usize = 100;
const BODY_CHARS: usize = 1200;

/// What every warning ends with.
const EXPERIMENTAL: &str = "Reported only: this gate is experimental and never fails until \
     owner policy makes it required. `weighin ci-check` reports the open items (04 §9 rule 1) and \
     the journals' append-only check (rule 5); decision corroboration (rule 2) and the evidence \
     rule (rule 3) are not checked, and G2 is enforced by CP1 (`assay check --local`) and by \
     convention (settle Weigh-In before the session ends).";

/// A warning's text, clipped to the schema's caps when it becomes a finding.
struct Text {
    evidence: String,
    title: String,
    body: String,
}

/// The document's findings: journal violations, then blocking items, then items pending
/// triage, at most [`MAX_FINDINGS`] and a note for the rest. Only the findings listed are built.
pub(super) fn warnings(report: &Report) -> Vec<Finding> {
    let standings: Vec<(&OpenItem, Standing)> = report
        .open_items
        .iter()
        .map(|item| (item, item_standing(item)))
        .collect();
    let reported = standings
        .iter()
        .filter(|(_, standing)| *standing != Standing::Clear)
        .count();
    let blocking = standings
        .iter()
        .filter_map(|(item, standing)| match standing {
            Standing::Blocking(causes) => Some(blocking_item(item, *causes)),
            Standing::PendingTriage | Standing::Clear => None,
        });
    let pending = standings
        .iter()
        .filter(|(_, standing)| *standing == Standing::PendingTriage)
        .map(|(item, _)| pending_item(item));
    let mut findings: Vec<Finding> = report
        .violations
        .iter()
        .map(rewritten)
        .chain(blocking)
        .chain(pending)
        .take(MAX_FINDINGS)
        .collect();
    let left = report
        .violations
        .len()
        .saturating_add(reported)
        .saturating_sub(MAX_FINDINGS);
    if left > 0 {
        findings.push(truncated(left));
    }
    findings
}

/// Where `item` stands by 04 §9 rule 1: `blocks: none` (a date deadline only) is the only
/// value that holds up nothing; `unknown` blocks, never reads as none.
fn item_standing(item: &OpenItem) -> Standing {
    let blocks = match item.blocks {
        Blocks::None => assay_domain::Blocks::Unstated,
        Blocks::Merge | Blocks::Release | Blocks::Unknown => assay_domain::Blocks::Stated,
    };
    standing(item.triage, blocks)
}

/// The warning for a journal that breaks the append-only rule, at its line (line 1 when the
/// violation has none: a removed journal).
fn rewritten(violation: &Violation) -> Finding {
    let path = plain_name(&violation.path);
    let at = violation
        .line
        .map_or_else(String::new, |line| format!(" at line {line}"));
    let reason = violation.reason.code();
    let what = match violation.reason {
        Reason::Removed => "is gone at head",
        Reason::Missing => "no longer holds the event of a complete base line",
        Reason::Altered => "holds another event where a complete base line's was",
        Reason::Undecodable => "holds a complete line that is not a Weigh-In event",
    };
    warning(
        (JOURNAL_REWRITTEN, Severity::Major),
        (&violation.path, violation.line.unwrap_or(NonZeroU64::MIN)),
        &Text {
            evidence: format!("{path}{at}: {reason}"),
            title: format!("Weigh-In journal rewritten: `{path}`"),
            body: format!(
                "`{path}` {what}{at} (`{reason}`): Weigh-In journals are append-only, so every \
                 complete line a journal held at base must still be there at head (04 §9 rule \
                 5). {EXPERIMENTAL}"
            ),
        },
    )
}

/// The warning for an open, triaged item that blocks, naming each cause.
fn blocking_item(item: &OpenItem, causes: BlockingCauses) -> Finding {
    let id = item.id.as_str();
    let status = item.status.code();
    let reasons: Vec<String> = causes.iter().map(|cause| reason(cause, item)).collect();
    let (door, authority) = item.triage.map_or(("-", "-"), |triage| {
        (triage.door.code(), triage.authority.code())
    });
    warning(
        (OPEN_BLOCKING, Severity::Major),
        (JOURNALS, NonZeroU64::MIN),
        &Text {
            evidence: format!(
                "{id} {status}, door {door}, authority {authority}, blocks {}",
                item.blocks.code()
            ),
            title: format!("Weigh-In item `{id}` is open and blocking"),
            body: format!(
                "`{id}` is {status}, and blocks because {} (04 §9 rule 1): it needs a decision \
                 before merge. {EXPERIMENTAL}",
                joined(&reasons)
            ),
        },
    )
}

/// How `cause` reads in a blocking item's body.
fn reason(cause: BlockingCause, item: &OpenItem) -> String {
    match cause {
        BlockingCause::Door(door) => format!(
            "its door is `{}`, not `{}`",
            door.code(),
            Door::TwoWay.code()
        ),
        BlockingCause::Authority => format!(
            "its authority is `{}`, not `{}`",
            Authority::Authority.code(),
            Authority::Judgment.code()
        ),
        BlockingCause::Blocks => format!(
            "it blocks `{}`, not `{}`",
            item.blocks.code(),
            Blocks::None.code()
        ),
    }
}

/// `parts` as a list: `a`, `a and b`, `a, b, and c`.
fn joined(parts: &[String]) -> String {
    match parts {
        [first, second] => format!("{first} and {second}"),
        [rest @ .., last] if rest.len() > 1 => format!("{}, and {last}", rest.join(", ")),
        _ => parts.concat(),
    }
}

/// The warning for an open item whose door or authority is not triaged yet.
fn pending_item(item: &OpenItem) -> Finding {
    let id = item.id.as_str();
    let status = item.status.code();
    warning(
        (PENDING_TRIAGE, Severity::Minor),
        (JOURNALS, NonZeroU64::MIN),
        &Text {
            evidence: format!("{id} {status}, not triaged"),
            title: format!("Weigh-In item `{id}` awaits triage"),
            body: format!(
                "`{id}` is {status} and its triaged door or authority is still null: an \
                 untriaged item is never treated as non-blocking (04 §9 rule 1); run \
                 `/weigh-in` to triage it. {EXPERIMENTAL}"
            ),
        },
    )
}

/// The note that `left` findings were not listed.
fn truncated(left: usize) -> Finding {
    warning(
        (TRUNCATED, Severity::Minor),
        (JOURNALS, NonZeroU64::MIN),
        &Text {
            evidence: format!("{left} more not listed"),
            title: "Weigh-In findings truncated".to_owned(),
            body: format!(
                "{left} more Weigh-In findings are not listed: the document lists at most \
                 {MAX_FINDINGS}. {EXPERIMENTAL}"
            ),
        },
    )
}

/// A warning under `rule` at `(file, line)` at head.
fn warning(
    (rule, severity): (&str, Severity),
    (file, line): (&str, NonZeroU64),
    text: &Text,
) -> Finding {
    Finding {
        class: Class::Compliance,
        rule: Some(rule.to_owned()),
        kind: FindingKind::Defect,
        severity,
        introduced_by: None,
        pre_existing: false,
        file: file.to_owned(),
        line,
        end_line: None,
        side: Some(Side::Right),
        evidence: clip_chars(&text.evidence, EVIDENCE_CHARS),
        expected_behavior: None,
        trigger: None,
        caller: None,
        title: clip_chars(&text.title, TITLE_CHARS),
        body: clip_chars(&text.body, BODY_CHARS),
        fix: None,
        confidence: None,
        group_hint: None,
    }
}
