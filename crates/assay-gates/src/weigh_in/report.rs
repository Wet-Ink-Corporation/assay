//! The `weighin.ci-check/1` report `weighin ci-check --json` prints (Weigh-In 1.1.0 and later,
//! `weighin/src/ci/report.rs`), read strictly (04 §9 CP2).
//!
//! It is the output of a tool that read the PR's files, so it is untrusted (wi-b5affa): it is
//! read under the contract document cap, a repeated key is refused, the `schema` must be exactly
//! [`SCHEMA`] before anything else is read (an unknown version is refused, never guessed at),
//! and every field assay reads or checks must be present with a value weighin writes, or the
//! whole report is refused. Fields weighin adds within the version are ignored (the schema is
//! additive within a version). `question`, `resolve_by` and a decision's `option` are checked
//! for their shape and kept nowhere ([`Unquoted`]), so no finding can quote them. A refusal
//! says what was wrong without quoting the report.

use std::num::NonZeroU64;

use assay_app::{CommitId, MAX_DOCUMENT_BYTES, parse_json_strict};
use assay_domain::{Authority, Door, Triage};
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use thiserror::Error;

use super::id::ItemId;
use crate::json::json_detail;

/// The schema name and version the report carries.
pub(super) const SCHEMA: &str = "weighin.ci-check/1";
/// Most open items read; a longer list is refused.
const MAX_OPEN_ITEMS: usize = 10_000;
/// Most journal violations read; a longer list is refused.
const MAX_VIOLATIONS: usize = 10_000;

/// What an open item holds up (`blocks`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Blocks {
    /// It must be settled before the branch merges.
    Merge,
    /// It must be settled before the next release.
    Release,
    /// It has a date deadline only, which holds up neither.
    None,
    /// Nothing recorded says.
    Unknown,
}

impl Blocks {
    /// The value as weighin writes it.
    pub(super) const fn code(self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Release => "release",
            Self::None => "none",
            Self::Unknown => "unknown",
        }
    }
}

/// Why a journal breaks the append-only rule (04 §9 rule 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Reason {
    /// The journal is gone at head.
    Removed,
    /// A complete base line's event is nowhere in the head journal.
    Missing,
    /// As `Missing`, and the head line at that number holds another event.
    Altered,
    /// A complete head line that is not an event.
    Undecodable,
}

impl Reason {
    /// The value as weighin writes it.
    pub(super) const fn code(self) -> &'static str {
        match self {
            Self::Removed => "removed",
            Self::Missing => "missing",
            Self::Altered => "altered",
            Self::Undecodable => "undecodable",
        }
    }
}

/// An item's status: one `Status::is_open` covers, the only ones `ci-check` lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Status {
    Raised,
    Triaged,
    Grounded,
    Presented,
    Deferred,
}

impl Status {
    /// The value as weighin writes it.
    pub(super) const fn code(self) -> &'static str {
        match self {
            Self::Raised => "raised",
            Self::Triaged => "triaged",
            Self::Grounded => "grounded",
            Self::Presented => "presented",
            Self::Deferred => "deferred",
        }
    }
}

/// An item's kind, read for its shape only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Question,
    Assumption,
    Deferral,
    Call,
}

/// Where a decision is attested, read for its shape only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Source {
    Journal,
    Atom,
    Trailer,
}

/// A string of the report checked for its shape and kept nowhere, so it cannot be quoted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(from = "String")]
struct Unquoted;

impl From<String> for Unquoted {
    fn from(_text: String) -> Self {
        Self
    }
}

/// `Option<T>` that must be present (null or a value): a field weighin always writes.
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    Option::deserialize(deserializer)
}

#[derive(Deserialize)]
struct ReportDto {
    base: String,
    head: String,
    open_items: Vec<OpenItemDto>,
    decisions: Vec<DecisionDto>,
    journal: JournalDto,
}

#[derive(Deserialize)]
struct OpenItemDto {
    id: String,
    status: Status,
    // Checked for its closed set, not reported.
    #[serde(rename = "kind")]
    _kind: Kind,
    #[serde(deserialize_with = "present")]
    door: Option<Door>,
    #[serde(deserialize_with = "present")]
    authority: Option<Authority>,
    blocks: Blocks,
    #[serde(deserialize_with = "present", rename = "resolve_by")]
    _resolve_by: Option<Unquoted>,
    #[serde(rename = "question")]
    _question: Unquoted,
}

#[derive(Deserialize)]
struct DecisionDto {
    id: String,
    #[serde(deserialize_with = "present", rename = "option")]
    _option: Option<Unquoted>,
    #[serde(deserialize_with = "present")]
    commit: Option<String>,
    sources: Vec<Source>,
}

#[derive(Deserialize)]
struct JournalDto {
    ok: bool,
    violations: Vec<ViolationDto>,
}

#[derive(Deserialize)]
struct ViolationDto {
    path: String,
    #[serde(deserialize_with = "present")]
    line: Option<NonZeroU64>,
    reason: Reason,
}

/// One open item at head, as assay reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OpenItem {
    pub(super) id: ItemId,
    pub(super) status: Status,
    /// The triaged door and authority; `None` until both are triaged.
    pub(super) triage: Option<Triage>,
    pub(super) blocks: Blocks,
}

/// One break of the append-only rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Violation {
    /// The journal, relative to the tree; never empty.
    pub(super) path: String,
    /// The 1-based line at fault, as `ci-check` numbers it: the base line for
    /// [`Reason::Missing`] and [`Reason::Altered`], the head line for [`Reason::Undecodable`],
    /// and none for [`Reason::Removed`].
    pub(super) line: Option<NonZeroU64>,
    pub(super) reason: Reason,
}

/// What assay reads of a report: its open items and its journal violations. Its decisions are
/// checked for their shape only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Report {
    pub(super) open_items: Vec<OpenItem>,
    pub(super) violations: Vec<Violation>,
}

/// Why a report was refused. Nothing of the report is quoted.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WeighInReportError {
    /// The output is over [`MAX_DOCUMENT_BYTES`]; it was not parsed.
    #[error(
        "weighin ci-check printed a report that is {len} bytes, over the {MAX_DOCUMENT_BYTES}-byte cap"
    )]
    TooLarge {
        /// Its length in bytes.
        len: usize,
    },
    /// Not one JSON document without repeated keys, or not the shape `weighin.ci-check/1`
    /// has: a field missing or holding a value weighin does not write there.
    #[error("weighin ci-check printed a report that is {detail}")]
    Json {
        /// What was wrong and where, without quoting it.
        detail: String,
    },
    /// The `schema` is not exactly `weighin.ci-check/1`.
    #[error("weighin ci-check printed a report that is not {SCHEMA}, the only version assay reads")]
    Schema,
    /// The report's `base` is not the base it was asked about.
    #[error("weighin ci-check printed a report that is about another base commit")]
    Base,
    /// The report's `head` is not the commit checked out.
    #[error("weighin ci-check printed a report that is about another head commit")]
    Head,
    /// An item or decision id is not a Weigh-In id.
    #[error("weighin ci-check printed a report that is holding an id weighin does not write")]
    Id,
    /// A decision's commit is not a full commit id.
    #[error("weighin ci-check printed a report that is holding a commit that is not a full id")]
    Commit,
    /// A decision names no source.
    #[error("weighin ci-check printed a report that is holding a decision with no source")]
    Sources,
    /// A violation's path is empty.
    #[error("weighin ci-check printed a report that is holding a violation with an empty path")]
    Path,
    /// `journal.ok` does not say whether there are violations.
    #[error(
        "weighin ci-check printed a report that is contradicting itself: journal.ok and its violations disagree"
    )]
    JournalOk,
    /// More than 10,000 open items.
    #[error(
        "weighin ci-check printed a report that is listing more than {MAX_OPEN_ITEMS} open items"
    )]
    TooManyItems,
    /// More than 10,000 violations.
    #[error(
        "weighin ci-check printed a report that is listing more than {MAX_VIOLATIONS} journal violations"
    )]
    TooManyViolations,
}

/// The report in `stdout`, which must be about `base` and `head`.
///
/// # Errors
///
/// [`WeighInReportError`] when it is over the document cap, not one JSON object without repeated
/// keys, of another schema, about another base or head, of another shape, or past a cap.
pub(super) fn read(
    stdout: &[u8],
    base: &CommitId,
    head: &CommitId,
) -> Result<Report, WeighInReportError> {
    if stdout.len() > MAX_DOCUMENT_BYTES {
        return Err(WeighInReportError::TooLarge { len: stdout.len() });
    }
    let value = parse_json_strict(stdout).map_err(|error| json(&error))?;
    if value.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(WeighInReportError::Schema);
    }
    // Read again from the bytes, so a refusal can say where; the strict parse above already
    // refused repeated keys.
    let report: ReportDto = serde_json::from_slice(stdout).map_err(|error| json(&error))?;
    if report.base != base.as_str() {
        return Err(WeighInReportError::Base);
    }
    if report.head != head.as_str() {
        return Err(WeighInReportError::Head);
    }
    if report.open_items.len() > MAX_OPEN_ITEMS {
        return Err(WeighInReportError::TooManyItems);
    }
    let journal = report.journal;
    if journal.violations.len() > MAX_VIOLATIONS {
        return Err(WeighInReportError::TooManyViolations);
    }
    if journal.ok != journal.violations.is_empty() {
        return Err(WeighInReportError::JournalOk);
    }
    for decision in report.decisions {
        check_decision(decision)?;
    }
    let open_items = report
        .open_items
        .into_iter()
        .map(open_item)
        .collect::<Result<_, _>>()?;
    let violations = journal
        .violations
        .into_iter()
        .map(violation)
        .collect::<Result<_, _>>()?;
    Ok(Report {
        open_items,
        violations,
    })
}

/// A JSON refusal, said without quoting the report.
fn json(error: &serde_json::Error) -> WeighInReportError {
    WeighInReportError::Json {
        detail: json_detail(error),
    }
}

fn open_item(item: OpenItemDto) -> Result<OpenItem, WeighInReportError> {
    Ok(OpenItem {
        id: ItemId::new(item.id).ok_or(WeighInReportError::Id)?,
        status: item.status,
        triage: item
            .door
            .zip(item.authority)
            .map(|(door, authority)| Triage { door, authority }),
        blocks: item.blocks,
    })
}

fn check_decision(decision: DecisionDto) -> Result<(), WeighInReportError> {
    ItemId::new(decision.id).ok_or(WeighInReportError::Id)?;
    if decision
        .commit
        .is_some_and(|commit| !is_full_commit(&commit))
    {
        return Err(WeighInReportError::Commit);
    }
    if decision.sources.is_empty() {
        return Err(WeighInReportError::Sources);
    }
    Ok(())
}

fn violation(violation: ViolationDto) -> Result<Violation, WeighInReportError> {
    if violation.path.is_empty() {
        return Err(WeighInReportError::Path);
    }
    Ok(Violation {
        path: violation.path,
        line: violation.line,
        reason: violation.reason,
    })
}

/// Whether `text` is a full commit id as git prints it: 40 (SHA-1) or 64 (SHA-256) lowercase
/// hex digits.
fn is_full_commit(text: &str) -> bool {
    matches!(text.len(), 40 | 64)
        && text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// `weighin ci-check --json` from weighin 1.2.0 on a fixture repository: four open items
    /// (`wi-0000a1` one-way authority blocking merge, `wi-0000c3` two-way judgment with a date,
    /// `wi-0000d4` two-way judgment blocking release, `wi-0000f6` untriaged), one trailer
    /// decision, and two journal violations (a missing line 3, a removed journal).
    const RECORDED: &[u8] = include_bytes!("../../tests/fixtures/weigh-in/ci-check-1.2.0.json");
    const BASE: &str = "6c51ad64e08cdf6d8ffa0ffd06dd427776318acc";
    const HEAD: &str = "7133cd8fbaaaef75ce9a694312d2784946570a96";

    fn commit(text: &str) -> CommitId {
        text.parse().expect("a fixture commit")
    }

    fn read_recorded(bytes: &[u8]) -> Result<Report, WeighInReportError> {
        read(bytes, &commit(BASE), &commit(HEAD))
    }

    fn recorded() -> Value {
        serde_json::from_slice(RECORDED).expect("the fixture is JSON")
    }

    fn bytes(value: &Value) -> Vec<u8> {
        serde_json::to_vec(value).expect("a value encodes")
    }

    #[test]
    fn the_recorded_report_reads_its_items_and_violations() {
        let report = read_recorded(RECORDED).expect("the recorded report is read");

        let items: Vec<(&str, Option<Triage>, Blocks)> = report
            .open_items
            .iter()
            .map(|item| (item.id.as_str(), item.triage, item.blocks))
            .collect();
        let triage = |door, authority| Some(Triage { door, authority });
        assert_eq!(
            items,
            [
                (
                    "wi-0000a1",
                    triage(Door::OneWay, Authority::Authority),
                    Blocks::Merge
                ),
                (
                    "wi-0000c3",
                    triage(Door::TwoWay, Authority::Judgment),
                    Blocks::None
                ),
                (
                    "wi-0000d4",
                    triage(Door::TwoWay, Authority::Judgment),
                    Blocks::Release
                ),
                ("wi-0000f6", None, Blocks::Unknown),
            ]
        );
        assert_eq!(
            report.violations,
            [
                Violation {
                    path: ".kb/_ledger/journal/s1.jsonl".to_owned(),
                    line: NonZeroU64::new(3),
                    reason: Reason::Missing,
                },
                Violation {
                    path: ".kb/_ledger/journal/s2.jsonl".to_owned(),
                    line: None,
                    reason: Reason::Removed,
                },
            ]
        );
    }

    #[test]
    fn another_schema_is_refused_before_its_shape_is_read() {
        for schema in [
            json!("weighin.ci-check/2"),
            json!("weighin.ci-check/1 "),
            json!(1),
        ] {
            let other = json!({ "schema": schema, "open_items": "not a list" });

            let refused = read_recorded(&bytes(&other));

            assert!(
                matches!(refused, Err(WeighInReportError::Schema)),
                "{schema}: {refused:?}"
            );
        }
        assert!(matches!(
            read_recorded(b"{\"open_items\":[]}"),
            Err(WeighInReportError::Schema)
        ));
    }

    #[test]
    fn a_report_of_another_base_or_head_is_refused() {
        let other = commit("0123456789abcdef0123456789abcdef01234567");

        let base = read(RECORDED, &other, &commit(HEAD));
        let head = read(RECORDED, &commit(BASE), &other);

        assert!(matches!(base, Err(WeighInReportError::Base)), "{base:?}");
        assert!(matches!(head, Err(WeighInReportError::Head)), "{head:?}");
    }

    #[test]
    fn output_that_is_not_one_json_object_without_repeated_keys_is_refused() {
        let cases: [&[u8]; 4] = [
            b"",
            b"{\"schema\":\"weighin.ci-check/1\"",
            b"[]",
            b"{\"schema\":\"weighin.ci-check/1\",\"schema\":\"weighin.ci-check/1\"}",
        ];
        for case in cases {
            let refused = read_recorded(case);

            assert!(
                matches!(
                    refused,
                    Err(WeighInReportError::Json { .. } | WeighInReportError::Schema)
                ),
                "{:?}: {refused:?}",
                String::from_utf8_lossy(case)
            );
        }
    }

    #[test]
    fn output_over_the_document_cap_is_refused_unparsed() {
        let over = vec![b' '; MAX_DOCUMENT_BYTES + 1];

        assert!(matches!(
            read_recorded(&over),
            Err(WeighInReportError::TooLarge { len }) if len == MAX_DOCUMENT_BYTES + 1
        ));
    }

    /// The recorded report with `pointer`'s value replaced by `value`.
    fn with(pointer: &str, value: Value) -> Vec<u8> {
        let mut report = recorded();
        *report
            .pointer_mut(pointer)
            .expect("the pointer names a field") = value;
        bytes(&report)
    }

    #[test]
    fn a_value_weighin_does_not_write_is_refused() {
        let cases = [
            ("/open_items/0/status", json!("decided")),
            ("/open_items/0/status", json!("Triaged")),
            ("/open_items/0/kind", json!("todo")),
            ("/open_items/0/door", json!("sideways")),
            ("/open_items/0/authority", json!("agent")),
            ("/open_items/0/blocks", json!("deploy")),
            ("/open_items/0/blocks", Value::Null),
            ("/open_items/0/resolve_by", json!(7)),
            ("/open_items/0/question", Value::Null),
            ("/decisions/0/sources/0", json!("chat")),
            ("/decisions/0/option", json!(2)),
            ("/journal/violations/0/line", json!(0)),
            ("/journal/violations/0/line", json!(-1)),
            ("/journal/violations/0/reason", json!("moved")),
            ("/journal/ok", json!("false")),
            ("/open_items", json!({})),
        ];
        for (pointer, value) in cases {
            let refused = read_recorded(&with(pointer, value.clone()));

            assert!(
                matches!(refused, Err(WeighInReportError::Json { .. })),
                "{pointer} = {value}: {refused:?}"
            );
        }
    }

    #[test]
    fn a_field_weighin_always_writes_that_is_missing_is_refused() {
        let fields = [
            ("/open_items/0", "door"),
            ("/open_items/0", "authority"),
            ("/open_items/0", "resolve_by"),
            ("/open_items/0", "question"),
            ("/open_items/0", "blocks"),
            ("/decisions/0", "option"),
            ("/decisions/0", "commit"),
            ("/journal/violations/1", "line"),
            ("/journal", "violations"),
            ("", "decisions"),
        ];
        for (object, field) in fields {
            let mut report = recorded();
            report
                .pointer_mut(object)
                .and_then(Value::as_object_mut)
                .expect("the pointer names an object")
                .remove(field);

            let refused = read_recorded(&bytes(&report));

            assert!(
                matches!(refused, Err(WeighInReportError::Json { .. })),
                "without {object}/{field}: {refused:?}"
            );
        }
    }

    #[test]
    fn an_id_commit_or_path_weighin_would_not_write_is_refused() {
        let cases = [
            (
                "/open_items/1/id",
                json!("wi-0000C3"),
                WeighInReportError::Id,
            ),
            ("/decisions/0/id", json!("0000e5"), WeighInReportError::Id),
            (
                "/decisions/0/commit",
                json!("7133cd8"),
                WeighInReportError::Commit,
            ),
            (
                "/journal/violations/0/path",
                json!(""),
                WeighInReportError::Path,
            ),
        ];
        for (pointer, value, expected) in cases {
            let refused = read_recorded(&with(pointer, value.clone()));

            assert_eq!(refused.err(), Some(expected), "{pointer} = {value}");
        }
    }

    #[test]
    fn a_decision_without_a_source_is_refused() {
        let refused = read_recorded(&with("/decisions/0/sources", json!([])));

        assert_eq!(refused.err(), Some(WeighInReportError::Sources));
    }

    #[test]
    fn journal_ok_must_say_whether_there_are_violations() {
        let ok_with_violations = read_recorded(&with("/journal/ok", json!(true)));
        let mut clean = recorded();
        clean["journal"]["violations"] = json!([]);
        let not_ok_without = read_recorded(&bytes(&clean));
        clean["journal"]["ok"] = json!(true);
        let ok_without = read_recorded(&bytes(&clean));

        assert_eq!(
            ok_with_violations.err(),
            Some(WeighInReportError::JournalOk)
        );
        assert_eq!(not_ok_without.err(), Some(WeighInReportError::JournalOk));
        assert!(ok_without.is_ok_and(|report| report.violations.is_empty()));
    }

    #[test]
    fn more_items_or_violations_than_the_caps_are_refused() {
        let mut report = recorded();
        let item = report["open_items"][3].clone();
        report["open_items"] = Value::Array(vec![item; MAX_OPEN_ITEMS + 1]);
        let items = read_recorded(&bytes(&report));
        report["open_items"] = json!([]);
        let violation = report["journal"]["violations"][0].clone();
        report["journal"]["violations"] = Value::Array(vec![violation; MAX_VIOLATIONS + 1]);
        let violations = read_recorded(&bytes(&report));
        report["journal"]["violations"]
            .as_array_mut()
            .expect("a list")
            .pop();
        let at_cap = read_recorded(&bytes(&report));

        assert_eq!(items.err(), Some(WeighInReportError::TooManyItems));
        assert_eq!(
            violations.err(),
            Some(WeighInReportError::TooManyViolations)
        );
        assert!(at_cap.is_ok_and(|report| report.violations.len() == MAX_VIOLATIONS));
    }

    #[test]
    fn fields_weighin_adds_within_the_version_are_ignored() {
        let mut report = recorded();
        report["extra"] = json!({ "any": [1, 2] });
        report["open_items"][0]["severity"] = json!("high");
        report["journal"]["violations"][0]["note"] = json!(null);

        let read = read_recorded(&bytes(&report));

        assert!(read.is_ok_and(|report| report.open_items.len() == 4));
    }

    #[test]
    fn a_refusal_never_quotes_the_report() {
        let refused = read_recorded(&with("/open_items/0/door", json!("x`[l](https://e)")))
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();

        assert!(
            refused.starts_with("weighin ci-check printed a report that is"),
            "{refused}"
        );
        assert!(!refused.contains("https"), "{refused}");
    }
}
