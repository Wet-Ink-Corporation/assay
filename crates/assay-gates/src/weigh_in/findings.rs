//! The adapter's warnings. None decides anything: the gate is not required until D-WI (04 §9),
//! and `decide` reads only required gates (rubric rule 1). Ids are validated hex; every other
//! PR-controlled name (a path, a triaged value) goes through [`plain_name`] into a code span.
//! Questions and other free text are never quoted.

use std::collections::HashMap;
use std::num::NonZeroU64;

use assay_app::{clip_chars, plain_name};
use assay_domain::{Class, Finding, FindingKind, Severity, Side};

use super::id::ItemId;
use super::journal::Blocks;
use super::ledger::{Item, JUDGMENT, Standing, TWO_WAY};

/// Most item and file findings in one document; one more says how many were left out.
const MAX_FINDINGS: usize = 1_000;

/// Rule of an open, triaged item that blocks (04 §9 rule 1).
const OPEN_BLOCKING: &str = "weigh-in-open-blocking";
/// Rule of an open item not triaged yet (the reason 04 §9 names).
const PENDING_TRIAGE: &str = "weigh-in-pending-triage";
/// Rule of a journal or atom refused whole.
const UNREADABLE: &str = "weigh-in-unreadable";
/// Rule of the note that findings were left out.
const TRUNCATED: &str = "weigh-in-truncated";

/// Most characters of `evidence`, `title` and `body` (03 §3).
const EVIDENCE_CHARS: usize = 400;
const TITLE_CHARS: usize = 100;
const BODY_CHARS: usize = 1200;

/// What every warning ends with.
const EXPERIMENTAL: &str = "Reported only: until Weigh-In ships `ci-check` (D-WI, #44) this \
     gate is experimental and never fails, and G2 is enforced by CP1 (`assay check --local`) \
     and by convention (settle Weigh-In before the session ends).";

/// A file the adapter refused, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Refused<'a> {
    pub(super) path: &'a str,
    /// The line at fault, when one is.
    pub(super) line: Option<NonZeroU64>,
    /// Completes "the file ...".
    pub(super) why: &'static str,
}

/// A warning's text, clipped to the schema's caps when it becomes a finding.
struct Text {
    evidence: String,
    title: String,
    body: String,
}

/// The document's findings: refused files, then blocking items, then items pending triage,
/// at most [`MAX_FINDINGS`] and a note for the rest. `atoms` maps an item to the atom that
/// names it. Only the findings listed are built.
pub(super) fn warnings(
    refused: &[Refused<'_>],
    items: &[Item<'_>],
    atoms: &HashMap<&ItemId, &str>,
) -> Vec<Finding> {
    let standings: Vec<(&Item<'_>, Standing<'_>)> =
        items.iter().map(|item| (item, item.standing())).collect();
    let reported = standings
        .iter()
        .filter(|(_, standing)| *standing != Standing::Settled)
        .count();
    let blocking = standings
        .iter()
        .filter_map(|(item, standing)| match *standing {
            Standing::Blocking { door, authority } => {
                Some(blocking_item(item, door, authority, atoms))
            }
            Standing::PendingTriage | Standing::Settled => None,
        });
    let pending = standings
        .iter()
        .filter(|(_, standing)| *standing == Standing::PendingTriage)
        .map(|(item, _)| pending_item(item, atoms));
    let mut findings: Vec<Finding> = refused
        .iter()
        .map(unreadable)
        .chain(blocking)
        .chain(pending)
        .take(MAX_FINDINGS)
        .collect();
    let left = refused
        .len()
        .saturating_add(reported)
        .saturating_sub(MAX_FINDINGS);
    if left > 0 {
        findings.push(truncated(left));
    }
    findings
}

/// The warning for a file refused whole, or for a ledger past the adapter's limits.
fn unreadable(refused: &Refused<'_>) -> Finding {
    let path = plain_name(refused.path);
    let at = refused
        .line
        .map_or_else(String::new, |line| format!(" (line {line})"));
    warning(
        (UNREADABLE, Severity::Minor),
        (refused.path, refused.line.unwrap_or(NonZeroU64::MIN)),
        &Text {
            evidence: format!("not read: {path}"),
            title: format!("Weigh-In file not read: `{path}`"),
            body: format!(
                "`{path}` {}{at}, so the adapter refused it whole rather than guess \
                 (wi-b5affa): the items it records are missing from this report. {EXPERIMENTAL}",
                refused.why
            ),
        },
    )
}

/// The warning for an open, triaged item that blocks.
fn blocking_item(
    item: &Item<'_>,
    door: &str,
    authority: &str,
    atoms: &HashMap<&ItemId, &str>,
) -> Finding {
    let (shown_door, shown_authority) = (plain_name(door), plain_name(authority));
    let mut reasons = Vec::new();
    match item.blocks {
        Blocks::Nothing => {}
        Blocks::Set => reasons.push("its `blocks` is set".to_owned()),
        Blocks::Unknown => reasons.push("no `item_raised` says what it blocks".to_owned()),
    }
    if door != TWO_WAY {
        reasons.push(format!("its door is `{shown_door}`, not `{TWO_WAY}`"));
    }
    if authority != JUDGMENT {
        reasons.push(format!(
            "its authority is `{shown_authority}`, not `{JUDGMENT}`"
        ));
    }
    let id = item.id;
    let status = status(item);
    warning(
        (OPEN_BLOCKING, Severity::Major),
        (item.path, item.line),
        &Text {
            evidence: format!("{id} {status}, door {shown_door}, authority {shown_authority}"),
            title: format!("Weigh-In item `{id}` is open and blocking"),
            body: format!(
                "`{id}` is {status} and triaged, and blocks because {} (04 §9 rule 1): it \
                 needs a decision before merge.{} {EXPERIMENTAL}",
                reasons.join(" and "),
                atom_note(id, atoms)
            ),
        },
    )
}

/// The warning for an open item whose door or authority is not triaged yet.
fn pending_item(item: &Item<'_>, atoms: &HashMap<&ItemId, &str>) -> Finding {
    let id = item.id;
    let status = status(item);
    warning(
        (PENDING_TRIAGE, Severity::Minor),
        (item.path, item.line),
        &Text {
            evidence: format!("{id} {status}, not triaged"),
            title: format!("Weigh-In item `{id}` awaits triage"),
            body: format!(
                "`{id}` is {status} and its triaged door or authority is still null: an \
                 untriaged item is never treated as non-blocking (04 §9 rule 1); run \
                 `/weigh-in` to triage it.{} {EXPERIMENTAL}",
                atom_note(id, atoms)
            ),
        },
    )
}

/// The note that `left` findings were not listed.
fn truncated(left: usize) -> Finding {
    warning(
        (TRUNCATED, Severity::Minor),
        (super::JOURNALS, NonZeroU64::MIN),
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

/// The item's status as the findings say it.
fn status(item: &Item<'_>) -> &'static str {
    item.status
        .map_or("open (status unknown)", |status| status.word())
}

/// A sentence naming the atom that records a decision for `id`, if there is one.
fn atom_note(id: &ItemId, atoms: &HashMap<&ItemId, &str>) -> String {
    atoms.get(id).map_or_else(String::new, |path| {
        format!(
            " The decision atom `{}` names it, but the journal does not record the decision, \
             and an atom alone proves nothing (04 §9 rule 2).",
            plain_name(path)
        )
    })
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
