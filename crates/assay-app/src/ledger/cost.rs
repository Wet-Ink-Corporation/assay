//! Cost records, `assay.ledger-cost/1`: one JSON line per record in `ledger/YYYY-MM.jsonl`,
//! keyed by `run_uid` and kind (06 §1, R3-06, R3-16).

use std::borrow::Cow;
use std::collections::HashSet;
use std::num::NonZeroU64;

use assay_domain::{Aeu, RunUid, Tier};
use serde::{Deserialize, Serialize};

use super::aeu_text::parse_aeu;
use super::budget::BUDGET_WINDOW;
use super::cas::{Next, append_line, read_file};
use super::error::{Corruption, LedgerReadError, LedgerWriteError};
use super::lines::{Newline, Refusal, check_canonical, check_schema, lines, parse, to_line};
use super::port::{Ledger, LedgerPath};
use super::record::{Appended, field, is_repository};
use super::time::{LedgerMonth, LedgerTime};
use crate::config::{CommitId, Sha256Digest};
use crate::prompt_bundle::Role;

/// The contract id every cost record carries.
const COST_SCHEMA: &str = "assay.ledger-cost/1";

/// What a run spent, or reserved, in the cost ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CostRecord {
    /// The run; with `kind`, the record's key.
    pub run_uid: RunUid,
    /// The run's evidence key, `sha256:<hex>` (03 §4 `run_key`).
    pub run_key: Sha256Digest,
    /// The consumer repository, `owner/name`.
    pub repo: String,
    /// The PR number.
    pub pr: NonZeroU64,
    /// The head commit reviewed.
    pub head_sha: CommitId,
    /// The tier the run spent at: `full`, `lite` or `micro`, the only tiers with a model stage.
    pub tier: Tier,
    /// What each role spent (a `final` record) or was reserved (a `reserved` one), at most once
    /// per role.
    pub roles: Vec<RoleCost>,
    /// The run's total: the reservation, the real cost, or the run cap charged for a run that
    /// died or was cancelled.
    pub aeu: Aeu,
    /// Whether this is the admission's reservation or the settled cost.
    pub kind: CostKind,
    /// A `final` record charged at the run cap because the review died, was cancelled, or was
    /// back-filled by `reconcile`. Never set on a `reserved` record.
    pub cancelled: bool,
    /// When the record was written; its month names the file it is in.
    pub at: LedgerTime,
}

/// Which of a run's two cost records this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CostKind {
    /// Written by `admit` when it acquires a slot; counts until the `final` record replaces it.
    Reserved,
    /// Written by `release` with the real cost, or by `reconcile` as a back-fill.
    Final,
}

/// What one model-calling stage spent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RoleCost {
    /// The stage.
    pub role: CostRole,
    /// What it spent, or was reserved.
    pub aeu: Aeu,
}

/// A stage that calls the model: a reviewer role, the coordinator or the verifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CostRole {
    /// A reviewer role (04 §3).
    Role(Role),
    /// The coordinator (`coordinator`).
    Coordinator,
    /// The verifier (`verifier`).
    Verifier,
}

impl CostRole {
    /// Every stage, reviewer roles in roster order first.
    pub const ALL: [Self; 8] = [
        Self::Role(Role::Bugs),
        Self::Role(Role::Security),
        Self::Role(Role::Design),
        Self::Role(Role::Meta),
        Self::Role(Role::Combined),
        Self::Role(Role::CombinedMicro),
        Self::Coordinator,
        Self::Verifier,
    ];

    /// The stage's wire code: the role's code, `coordinator` or `verifier`.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Role(role) => role.code(),
            Self::Coordinator => "coordinator",
            Self::Verifier => "verifier",
        }
    }

    /// The stage whose code is `code`.
    fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|role| role.code() == code)
    }
}

/// Appends `record` to its month's cost file, unless a record with its `run_uid` and kind is
/// already there: a back-fill or a repeated `release` writes nothing (R3-16).
///
/// # Errors
///
/// [`LedgerWriteError`]: the file could not be read or is refused, the write failed or kept
/// conflicting, the file would pass its cap, or `record` breaks a rule of its contract (for
/// example a `repo` that is not `owner/name`).
pub fn append_cost<L: Ledger>(
    ledger: &L,
    record: &CostRecord,
) -> Result<Appended<CostRecord>, LedgerWriteError<L::Error>> {
    let month = record.at.month();
    let path = LedgerPath::costs(month);
    let line = to_line(&CostLine::from(record)).map_err(|problem| LedgerWriteError::Invalid {
        path: path.clone(),
        problem,
    })?;
    let key = (record.run_uid, record.kind);
    if let Some(found) = earlier_month_record(ledger, record)? {
        return Ok(Appended::AlreadyRecorded(found));
    }
    append_line(
        ledger,
        &path,
        |contents| parse_costs(month, contents),
        |existing| {
            let found = existing
                .into_iter()
                .find(|found| key == (found.run_uid, found.kind));
            Ok(match found {
                Some(found) => Next::Keep(Appended::AlreadyRecorded(found)),
                None => Next::Append(Cow::Borrowed(&line), Appended::Recorded),
            })
        },
    )
}

/// The record with `record`'s `run_uid` and kind in the month before `record`'s, when the
/// [`BUDGET_WINDOW`] before `record.at` reaches into it: a run's records are written within a day
/// of each other, so a back-fill just after a month boundary still finds the `final` record
/// `release` wrote just before it (R3-16). That file is no longer appended to, so it is read once,
/// not by compare-and-set.
fn earlier_month_record<L: Ledger>(
    ledger: &L,
    record: &CostRecord,
) -> Result<Option<CostRecord>, LedgerWriteError<L::Error>> {
    let earlier = record.at.saturating_sub(BUDGET_WINDOW).month();
    if earlier == record.at.month() {
        return Ok(None);
    }
    let path = LedgerPath::costs(earlier);
    let Some(file) = read_file(ledger, &path).map_err(LedgerWriteError::Read)? else {
        return Ok(None);
    };
    let records = parse_costs(earlier, &file.contents).map_err(|(line, problem)| {
        LedgerWriteError::Read(LedgerReadError::Corrupt {
            path,
            line,
            problem,
        })
    })?;
    Ok(records
        .into_iter()
        .find(|found| (found.run_uid, found.kind) == (record.run_uid, record.kind)))
}

/// The cost records of `month`'s file `contents`, refused whole when any line breaks a rule: a
/// record of another month, or a second record with an earlier one's `run_uid` and kind.
pub(super) fn parse_costs(month: LedgerMonth, contents: &[u8]) -> Result<Vec<CostRecord>, Refusal> {
    let lines = lines(contents)?;
    let mut keys = HashSet::with_capacity(lines.len());
    let mut records = Vec::with_capacity(lines.len());
    for (number, line) in lines {
        let record = parse_cost(line).map_err(|problem| (number, problem))?;
        if record.at.month() != month {
            return Err((number, Corruption::WrongMonth));
        }
        if !keys.insert((record.run_uid, record.kind)) {
            return Err((number, Corruption::Duplicate));
        }
        records.push(record);
    }
    Ok(records)
}

/// One cost line as a record.
fn parse_cost(line: &[u8]) -> Result<CostRecord, Corruption> {
    let record = CostRecord::try_from(parse::<CostLine<'_>>(line)?)?;
    check_canonical(line, &CostLine::from(&record), Newline::Stripped)?;
    Ok(record)
}

/// The wire form of a cost record (OUT-01).
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CostLine<'a> {
    schema: Cow<'static, str>,
    run_uid: RunUid,
    run_key: String,
    repo: Cow<'a, str>,
    pr: NonZeroU64,
    head_sha: String,
    tier: Tier,
    roles: Vec<RoleLine>,
    aeu: String,
    kind: CostKind,
    cancelled: bool,
    at: String,
}

/// The wire form of one stage's cost.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RoleLine {
    role: String,
    aeu: String,
}

impl<'a> From<&'a CostRecord> for CostLine<'a> {
    fn from(record: &'a CostRecord) -> Self {
        Self {
            schema: Cow::Borrowed(COST_SCHEMA),
            run_uid: record.run_uid,
            run_key: record.run_key.to_string(),
            repo: Cow::Borrowed(&record.repo),
            pr: record.pr,
            head_sha: record.head_sha.to_string(),
            tier: record.tier,
            roles: record
                .roles
                .iter()
                .map(|role| RoleLine {
                    role: role.role.code().to_owned(),
                    aeu: role.aeu.to_string(),
                })
                .collect(),
            aeu: record.aeu.to_string(),
            kind: record.kind,
            cancelled: record.cancelled,
            at: record.at.to_string(),
        }
    }
}

impl TryFrom<CostLine<'_>> for CostRecord {
    type Error = Corruption;

    fn try_from(line: CostLine<'_>) -> Result<Self, Corruption> {
        check_schema(&line.schema, COST_SCHEMA)?;
        if !is_repository(&line.repo) {
            return Err(Corruption::Field("repo"));
        }
        if !matches!(line.tier, Tier::Full | Tier::Lite | Tier::Micro) {
            return Err(Corruption::Field("tier"));
        }
        if line.kind == CostKind::Reserved && line.cancelled {
            return Err(Corruption::Field("cancelled"));
        }
        Ok(Self {
            run_uid: line.run_uid,
            run_key: field(&line.run_key, "run_key")?,
            repo: line.repo.into_owned(),
            pr: line.pr,
            head_sha: field(&line.head_sha, "head_sha")?,
            tier: line.tier,
            roles: roles(line.roles)?,
            aeu: parse_aeu(&line.aeu).ok_or(Corruption::Field("aeu"))?,
            kind: line.kind,
            cancelled: line.cancelled,
            at: field(&line.at, "at")?,
        })
    }
}

/// The stages of a record, each at most once.
fn roles(lines: Vec<RoleLine>) -> Result<Vec<RoleCost>, Corruption> {
    let mut seen = HashSet::with_capacity(lines.len());
    lines
        .into_iter()
        .map(|line| {
            let role = CostRole::from_code(&line.role).ok_or(Corruption::Field("roles"))?;
            if !seen.insert(role) {
                return Err(Corruption::Field("roles"));
            }
            let aeu = parse_aeu(&line.aeu).ok_or(Corruption::Field("roles"))?;
            Ok(RoleCost { role, aeu })
        })
        .collect()
}
