//! Per-PR state, `assay.ledger-state/1`: `state/<repo_id>/<pr>.json`, the authoritative state the
//! verdict check only projects (02 §5, 06 §1, ADR-23), changed only by compare-and-set.

use std::borrow::Cow;
use std::collections::HashSet;
use std::num::NonZeroU64;

use assay_domain::Tier;
use serde::{Deserialize, Serialize};

use super::cas::{Refused, Step, compare_and_set, read_file};
use super::error::{Corruption, LedgerReadError, LedgerWriteError};
use super::lines::{Newline, check_canonical, check_schema, parse, to_line};
use super::port::{Ledger, LedgerPath};
use super::record::{field, is_repository};
use super::time::LedgerTime;
use crate::config::{CommitId, Sha256Digest};
use crate::publish::{EvidencePointer, EvidenceRecord, LlmEvidence};
use crate::status::UserId;

/// The contract id the state document carries.
const STATE_SCHEMA: &str = "assay.ledger-state/1";

/// The most evidence records, and the most actor counters, a PR's state holds.
pub const MAX_STATE_ENTRIES: usize = 256;

/// A PR's state in the ledger. The empty state ([`Default`]) is what a PR without a state file
/// has.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrState {
    /// The pipeline's latest evidence bundle, written by `decide` (R3-08).
    pub evidence: Option<EvidencePointer>,
    /// decide-only's latest decision bundle (R3-08).
    pub decision: Option<DecisionPointer>,
    /// One record per evidence key, for `admit`'s reuse check (R3-15, R3-17); at most
    /// [`MAX_STATE_ENTRIES`], keys distinct.
    pub evidence_records: Vec<EvidenceRecord>,
    /// Why the LLM stage is deferred, and how often it was requeued (R3-11, R3-15).
    pub deferral: Option<DeferralRecord>,
    /// Commands counted per actor, for decide-only's rate limit (T9, T12); at most
    /// [`MAX_STATE_ENTRIES`], actors distinct.
    pub actor_counters: Vec<ActorCounter>,
    /// The publication register: which key the verdict check of which head was last written
    /// under, by which run (D2 of #61). Every verdict writer writes it under the per-head lock,
    /// before the check; decide-only writes a check only while it carries this key.
    pub published: Option<PublicationRegister>,
}

/// The publication register `{head_sha, key, run_id}` (D2 of #61).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationRegister {
    /// The head whose verdict check was written.
    pub head_sha: CommitId,
    /// The key it was written under: a decision key, or a refusal's.
    pub key: Sha256Digest,
    /// The run that wrote it: a pipeline run, or a decide-only one.
    pub run_id: NonZeroU64,
}

/// The decision pointer `{run_id, sha256}`, written by decide-only for its own bundle (02 §5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionPointer {
    /// The decide-only run that uploaded the bundle.
    pub run_id: NonZeroU64,
    /// The sha256 of the uploaded bundle file.
    pub sha256: Sha256Digest,
}

/// The deferral record `{repo, pr, head, count, resets_at, flag}` (06 §1): the `requeue` job
/// selects PRs from these, never from labels (R3-15).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferralRecord {
    /// The consumer repository, `owner/name`.
    pub repo: String,
    /// The PR number, its file's.
    pub pr: NonZeroU64,
    /// The head the deferral is for.
    pub head: CommitId,
    /// How many times the PR was requeued (at most 3 before `failure`, 03 §5).
    pub count: u32,
    /// When the rate limit resets, from a `rejected` rate-limit event, if one gave it.
    pub resets_at: Option<LedgerTime>,
    /// Set by a command, a label or a retry grant for the next `requeue` pass.
    pub flag: bool,
}

/// How many commands one actor issued on the PR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ActorCounter {
    /// The actor, by numeric id.
    pub actor_id: UserId,
    /// Their command count.
    pub count: u32,
}

/// What [`update_state`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateUpdate {
    /// The state was written; this is it now.
    Updated(PrState),
    /// The change left the state as it was, so nothing was written; this is it.
    Unchanged(PrState),
}

/// PR `pr`'s state; the empty state when there is no file.
///
/// # Errors
///
/// [`LedgerReadError`] when the file cannot be read, is over its cap, or breaks a rule.
pub fn read_state<L: Ledger>(
    ledger: &L,
    repo_id: NonZeroU64,
    pr: NonZeroU64,
) -> Result<PrState, LedgerReadError<L::Error>> {
    let path = LedgerPath::state(repo_id, pr);
    let Some(file) = read_file(ledger, &path)? else {
        return Ok(PrState::default());
    };
    parse_state(repo_id, pr, &file.contents).map_err(|problem| LedgerReadError::Corrupt {
        path,
        line: 1,
        problem,
    })
}

/// Applies `change` to PR `pr`'s state by compare-and-set.
///
/// `change` is shown the current state and returns the new one, or `None` to leave it. On a
/// conflict it is shown the fresh state and
/// asked again, so it must derive the new state from the one it is shown (an increment, not a
/// value computed before). A new state equal to the current one writes nothing.
///
/// # Errors
///
/// [`LedgerWriteError`]: the file could not be read or is refused, the write failed or kept
/// conflicting, the file would pass its cap, or the new state breaks a rule (more than
/// [`MAX_STATE_ENTRIES`] entries, a repeated key, a deferral record of another PR).
pub fn update_state<L: Ledger>(
    ledger: &L,
    repo_id: NonZeroU64,
    pr: NonZeroU64,
    mut change: impl FnMut(&PrState) -> Option<PrState>,
) -> Result<StateUpdate, LedgerWriteError<L::Error>> {
    let path = LedgerPath::state(repo_id, pr);
    compare_and_set(ledger, &path, |current| {
        let state = match current {
            Some(contents) => parse_state(repo_id, pr, contents)
                .map_err(|problem| Refused::Current((1, problem)))?,
            None => PrState::default(),
        };
        let next = match change(&state) {
            Some(next) if next != state => next,
            _ => return Ok(Step::Keep(StateUpdate::Unchanged(state))),
        };
        let document = to_line(&StateDocument::new(repo_id, pr, &next)).map_err(Refused::New)?;
        parse_state(repo_id, pr, &document).map_err(Refused::New)?;
        Ok(Step::Write(document, StateUpdate::Updated(next)))
    })
}

/// The state document `contents` of PR `pr`, refused when it breaks any rule.
fn parse_state(
    repo_id: NonZeroU64,
    pr: NonZeroU64,
    contents: &[u8],
) -> Result<PrState, Corruption> {
    let document = parse::<StateDocument<'_>>(contents)?;
    check_schema(&document.schema, STATE_SCHEMA)?;
    if (document.repo_id, document.pr) != (repo_id, pr) {
        return Err(Corruption::WrongPullRequest);
    }
    let state = PrState::try_from(document)?;
    if state
        .deferral
        .as_ref()
        .is_some_and(|deferral| deferral.pr != pr)
    {
        return Err(Corruption::WrongPullRequest);
    }
    check_canonical(
        contents,
        &StateDocument::new(repo_id, pr, &state),
        Newline::Included,
    )?;
    Ok(state)
}

/// The wire form of the state document (OUT-01). Absent parts are omitted.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StateDocument<'a> {
    schema: Cow<'static, str>,
    repo_id: NonZeroU64,
    pr: NonZeroU64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    evidence: Option<EvidenceLine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    decision: Option<DecisionLine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    published: Option<PublishedLine>,
    evidence_records: Vec<RecordLine<'a>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    deferral: Option<DeferralLine<'a>>,
    actor_counters: Vec<CounterLine>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublishedLine {
    head_sha: String,
    key: String,
    run_id: NonZeroU64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvidenceLine {
    run_id: NonZeroU64,
    run_attempt: NonZeroU64,
    sha256: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionLine {
    run_id: NonZeroU64,
    sha256: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordLine<'a> {
    evidence_key: Cow<'a, str>,
    bundle_sha256: String,
    llm: String,
    tier: Tier,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeferralLine<'a> {
    repo: Cow<'a, str>,
    pr: NonZeroU64,
    head: String,
    count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resets_at: Option<String>,
    flag: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CounterLine {
    actor_id: NonZeroU64,
    count: u32,
}

impl<'a> StateDocument<'a> {
    /// The document of `state` for PR `pr` of repository `repo_id`.
    fn new(repo_id: NonZeroU64, pr: NonZeroU64, state: &'a PrState) -> Self {
        Self {
            schema: Cow::Borrowed(STATE_SCHEMA),
            repo_id,
            pr,
            evidence: state.evidence.as_ref().map(|pointer| EvidenceLine {
                run_id: pointer.run_id,
                run_attempt: pointer.run_attempt,
                sha256: pointer.sha256.to_string(),
            }),
            decision: state.decision.as_ref().map(|pointer| DecisionLine {
                run_id: pointer.run_id,
                sha256: pointer.sha256.to_string(),
            }),
            published: state.published.as_ref().map(|register| PublishedLine {
                head_sha: register.head_sha.to_string(),
                key: register.key.to_string(),
                run_id: register.run_id,
            }),
            evidence_records: state
                .evidence_records
                .iter()
                .map(|record| RecordLine {
                    evidence_key: Cow::Borrowed(&record.evidence_key),
                    bundle_sha256: record.bundle_sha256.to_string(),
                    llm: record.llm.code().to_owned(),
                    tier: record.tier,
                })
                .collect(),
            deferral: state.deferral.as_ref().map(|deferral| DeferralLine {
                repo: Cow::Borrowed(&deferral.repo),
                pr: deferral.pr,
                head: deferral.head.to_string(),
                count: deferral.count,
                resets_at: deferral.resets_at.map(|time| time.to_string()),
                flag: deferral.flag,
            }),
            actor_counters: state
                .actor_counters
                .iter()
                .map(|counter| CounterLine {
                    actor_id: counter.actor_id.0,
                    count: counter.count,
                })
                .collect(),
        }
    }
}

impl TryFrom<StateDocument<'_>> for PrState {
    type Error = Corruption;

    fn try_from(document: StateDocument<'_>) -> Result<Self, Corruption> {
        if document.evidence_records.len() > MAX_STATE_ENTRIES {
            return Err(Corruption::Field("evidence_records"));
        }
        if document.actor_counters.len() > MAX_STATE_ENTRIES {
            return Err(Corruption::Field("actor_counters"));
        }
        let evidence = document
            .evidence
            .map(|line| -> Result<_, Corruption> {
                Ok(EvidencePointer {
                    run_id: line.run_id,
                    run_attempt: line.run_attempt,
                    sha256: field(&line.sha256, "evidence")?,
                })
            })
            .transpose()?;
        let decision = document
            .decision
            .map(|line| -> Result<_, Corruption> {
                Ok(DecisionPointer {
                    run_id: line.run_id,
                    sha256: field(&line.sha256, "decision")?,
                })
            })
            .transpose()?;
        let published = document
            .published
            .map(|line| -> Result<_, Corruption> {
                Ok(PublicationRegister {
                    head_sha: field(&line.head_sha, "published")?,
                    key: field(&line.key, "published")?,
                    run_id: line.run_id,
                })
            })
            .transpose()?;
        Ok(Self {
            evidence,
            decision,
            evidence_records: evidence_records(document.evidence_records)?,
            deferral: document.deferral.map(deferral).transpose()?,
            actor_counters: actor_counters(document.actor_counters)?,
            published,
        })
    }
}

/// The evidence records, their keys distinct.
fn evidence_records(lines: Vec<RecordLine<'_>>) -> Result<Vec<EvidenceRecord>, Corruption> {
    let mut keys = HashSet::with_capacity(lines.len());
    lines
        .into_iter()
        .map(|line| {
            let key: Sha256Digest = field(&line.evidence_key, "evidence_key")?;
            if !keys.insert(key) {
                return Err(Corruption::Duplicate);
            }
            let llm = [
                LlmEvidence::Complete,
                LlmEvidence::SkippedDraft,
                LlmEvidence::Deferred,
                LlmEvidence::Degraded,
            ]
            .into_iter()
            .find(|llm| llm.code() == line.llm)
            .ok_or(Corruption::Field("llm"))?;
            Ok(EvidenceRecord {
                evidence_key: line.evidence_key.into_owned(),
                bundle_sha256: field(&line.bundle_sha256, "bundle_sha256")?,
                llm,
                tier: line.tier,
            })
        })
        .collect()
}

/// The deferral record.
fn deferral(line: DeferralLine<'_>) -> Result<DeferralRecord, Corruption> {
    if !is_repository(&line.repo) {
        return Err(Corruption::Field("deferral"));
    }
    Ok(DeferralRecord {
        repo: line.repo.into_owned(),
        pr: line.pr,
        head: field(&line.head, "deferral")?,
        count: line.count,
        resets_at: line
            .resets_at
            .map(|time| field(&time, "deferral"))
            .transpose()?,
        flag: line.flag,
    })
}

/// The actor counters, their actors distinct.
fn actor_counters(lines: Vec<CounterLine>) -> Result<Vec<ActorCounter>, Corruption> {
    let mut actors = HashSet::with_capacity(lines.len());
    lines
        .into_iter()
        .map(|line| {
            if !actors.insert(line.actor_id) {
                return Err(Corruption::Duplicate);
            }
            Ok(ActorCounter {
                actor_id: UserId(line.actor_id),
                count: line.count,
            })
        })
        .collect()
}
