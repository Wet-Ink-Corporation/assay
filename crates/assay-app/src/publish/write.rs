//! Writing the verdict: the two-phase claim check around the per-head lock (02 §5, R3-09), the
//! bundle upload, the evidence or decision pointer and the publication register (S9, D2 of
//! #61), the check and the summary comment (S8).

mod record;

use std::borrow::Cow;

use assay_domain::{
    Aeu, Conclusion, Decision, DecisionInputs, LlmStage, NotRunReason, PrClass, Reason, ReasonCode,
    Rubric, Tier,
};

pub(super) use self::record::{Claimant, Site};
use super::ports::{
    CheckConclusion, CheckName, CheckRun, CheckState, CheckStatus, CheckUpdate, EvidenceEntry,
    EvidencePointer, EvidenceRecord, LlmEvidence, PublishLedger, VerdictForge,
};
use super::render::{self, Facts, Projection, SUMMARY_MARKER, Summary};
use super::waivers;
use super::{ForgeStep, PersistFailure, PublishError, Published, Stale, Target, Unlock, Written};
use crate::config::Sha256Digest;
use crate::eligibility::IgnoredApproval;
use crate::forge::{Deadline, LockedWrite};

/// Everything a decision's check update and evidence record are built from: what `decide` holds
/// in memory, or what `publish_persisted` read back from the bundle.
pub(super) struct Basis<'w> {
    /// Rubric v1, for the waivers the decision did not apply.
    pub(super) rubric: &'w Rubric,
    /// The decision inputs.
    pub(super) inputs: &'w DecisionInputs,
    /// The decision over them.
    pub(super) decision: &'w Decision,
    /// The tier the LLM stage ran at.
    pub(super) tier: Tier,
    /// The PR's classes: a persist failure fails the check when there is one.
    pub(super) classes: &'w [PrClass],
    /// What the LLM stage cost.
    pub(super) cost: Aeu,
    /// The approvals ignored for timing.
    pub(super) ignored_approvals: &'w [IgnoredApproval],
}

/// The persist step of a decision, run after the first read of the check allows a write and
/// before the lock: the bundle's digest once it is persisted, or why it is not.
pub(super) type Persist<'w, L, U> =
    &'w mut dyn FnMut() -> Result<Sha256Digest, PersistFailure<L, U>>;

/// What one writer publishes.
pub(super) enum Pending<'w, L, U> {
    /// A refusal, whose update is ready.
    Refused(CheckUpdate),
    /// A decision, whose update depends on whether persisting it succeeds.
    Decided {
        basis: Basis<'w>,
        persist: Persist<'w, L, U>,
    },
}

impl<L, U> Pending<'_, L, U> {
    /// The key the check carries once this is published.
    fn key(&self) -> &str {
        match self {
            Self::Refused(update) => &update.external_id,
            Self::Decided { basis, .. } => &basis.decision.key,
        }
    }
}

/// A pending publication with its bundle persisted, or why it was not.
enum Prepared<'w, L, U> {
    Refused(CheckUpdate),
    Decided {
        basis: Basis<'w>,
        persisted: Result<Sha256Digest, PersistFailure<L, U>>,
    },
}

/// What happened under the lock.
enum Locked<L, U> {
    Stale,
    Written {
        state: CheckState,
        persist: Option<PersistFailure<L, U>>,
    },
}

/// What happened under the lock, or the forge call that failed there.
type UnderLock<F, L, U> = Result<Locked<L, U>, (ForgeStep, F)>;

/// The ports a writer writes through: two borrows, so it copies whatever `F` and `L` are.
pub(super) struct Writer<'p, F, L> {
    pub(super) forge: &'p F,
    pub(super) ledger: &'p L,
}

impl<F, L> Clone for Writer<'_, F, L> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<F, L> Copy for Writer<'_, F, L> {}

/// What [`write`] returns: what was published, with `U` the persist step's error.
pub(super) type WriteOutcome<F, L, U> = Result<
    Published<<L as PublishLedger>::Error, U>,
    PublishError<<F as VerdictForge>::Error, <L as PublishLedger>::Error>,
>;

/// Publishes `pending` when the check is still the pipeline run `target`'s to write: it carries
/// the run's `pending:` stamp, or the key `pending` publishes under.
pub(super) fn write<F: VerdictForge, L: PublishLedger, U>(
    target: &Target<'_>,
    ports: Writer<'_, F, L>,
    pending: Pending<'_, L::Error, U>,
) -> WriteOutcome<F, L, U> {
    write_at(&Site::of(target), Claimant::Stamp, ports, pending)
}

/// Publishes `pending` at `site` when the check is still `claimant`'s to write.
pub(super) fn write_at<F: VerdictForge, L: PublishLedger, U>(
    site: &Site<'_>,
    claimant: Claimant<'_>,
    ports: Writer<'_, F, L>,
    pending: Pending<'_, L::Error, U>,
) -> WriteOutcome<F, L, U> {
    let key = checked_key((site, claimant), pending.key())?;
    let name = CheckName::for_base(site.base_ref, site.default_branch);
    let observed = match observe((site, claimant), ports.forge, &name, &key)? {
        Observation::Writable(observed) => observed,
        Observation::Unchanged => return Ok(Published::Unchanged),
        Observation::Foreign => {
            return Ok(Published::Yielded {
                stale: Stale::Foreign,
                unlock: Unlock::NotTaken,
            });
        }
    };
    // Persisted before the lock: it is slow, and an artifact nothing points to is harmless.
    let prepared = match pending {
        Pending::Refused(update) => Prepared::Refused(update),
        Pending::Decided { basis, persist } => Prepared::Decided {
            basis,
            persisted: persist(),
        },
    };
    let Some(lease) = ports
        .ledger
        .acquire(site.run_uid.repo_id, site.head_sha)
        .map_err(PublishError::Lock)?
    else {
        return Err(PublishError::LockBusy);
    };
    let locked = under_lock(
        (site, claimant, &key),
        ports,
        (&name, &observed),
        (&lease, prepared),
    );
    let unlock = match ports.ledger.release(lease) {
        Ok(()) => Unlock::Released,
        Err(error) => Unlock::Failed(error),
    };
    match locked {
        Ok(Locked::Stale) => Ok(Published::Yielded {
            stale: Stale::Moved,
            unlock,
        }),
        Ok(Locked::Written { state, persist }) => Ok(Published::Written(Written {
            state,
            persist,
            unlock,
        })),
        Err((step, source)) => Err(PublishError::Locked {
            step,
            source,
            unlock,
        }),
    }
}

/// `key` as the register records it, before any port call: refused unless it is a digest (a
/// stamp could be mistaken for it) and, for decide-only, unless the claim is of `site`'s head.
fn checked_key<F, L>(
    (site, claimant): (&Site<'_>, Claimant<'_>),
    key: &str,
) -> Result<Sha256Digest, PublishError<F, L>> {
    let key = key
        .parse()
        .map_err(|_not_a_digest| PublishError::DecisionKey)?;
    if claimant
        .claim()
        .is_some_and(|claim| &claim.register.head_sha != site.head_sha)
    {
        return Err(PublishError::ClaimHead);
    }
    Ok(key)
}

/// What the first read of the check allows.
enum Observation {
    /// The check is this writer's: carrying its claim, or `key` but not completed.
    Writable(CheckRun),
    /// The check is completed under `key` already: nothing to write.
    Unchanged,
    /// The check carries neither `key` nor the writer's claim: another run owns it.
    Foreign,
}

/// Reads the App check `name` on the head and judges it against `key` and `claimant`'s claim,
/// before any write or the lock.
fn observe<F: VerdictForge, E>(
    (site, claimant): (&Site<'_>, Claimant<'_>),
    forge: &F,
    name: &CheckName,
    key: &Sha256Digest,
) -> Result<Observation, PublishError<F::Error, E>> {
    // Before the lock: there is no lease to bound it yet.
    let observed = forge
        .find_check(site.head_sha, name, None)
        .map_err(PublishError::Observe)?
        .ok_or(PublishError::CheckMissing)?;
    Ok(if observed.external_id == key.as_str() {
        if observed.status == CheckStatus::Completed {
            Observation::Unchanged
        } else {
            Observation::Writable(observed)
        }
    } else if claimant.owns(&observed.external_id, site) {
        Observation::Writable(observed)
    } else {
        Observation::Foreign
    })
}

/// Re-reads the check under the lock `lease` and, when it is unchanged, records the evidence or
/// decision pointer and the publication register, and writes the check and the summary
/// comment. Every call carries the lease's deadline, so none sends or waits past it, and the
/// ledger is written only before it (#290, #332).
fn under_lock<F: VerdictForge, L: PublishLedger, U>(
    (site, claimant, key): (&Site<'_>, Claimant<'_>, &Sha256Digest),
    ports: Writer<'_, F, L>,
    (name, observed): (&CheckName, &CheckRun),
    (lease, prepared): (&L::Lease, Prepared<'_, L::Error, U>),
) -> UnderLock<F::Error, L::Error, U> {
    let deadline = ports.ledger.deadline(lease);
    let current = ports
        .forge
        .find_check(site.head_sha, name, Some(deadline))
        .map_err(|error| (ForgeStep::ReadCheck, error))?;
    if current.as_ref() != Some(observed) {
        return Ok(Locked::Stale);
    }
    let record::Recorded::Ready { update, persist } =
        record::record((site, claimant, key), (ports.ledger, lease), prepared)
    else {
        return Ok(Locked::Stale);
    };
    let write = LockedWrite {
        update: &update,
        deadline,
    };
    ports
        .forge
        .update_check(observed.id, write)
        .map_err(|error| (ForgeStep::UpdateCheck, error))?;
    upsert_summary(ports.forge, site.number, (&update.summary, deadline))?;
    Ok(Locked::Written {
        state: update.state,
        persist,
    })
}

/// The evidence pointer and record of an uploaded bundle (02 §5).
fn evidence_entry(site: &Site<'_>, basis: &Basis<'_>, digest: Sha256Digest) -> EvidenceEntry {
    let run_uid = site.run_uid;
    EvidenceEntry {
        repo_id: run_uid.repo_id,
        pr: site.number,
        pointer: EvidencePointer {
            run_id: run_uid.run_id,
            run_attempt: run_uid.run_attempt,
            // The pointer and the record each own the digest.
            sha256: digest.clone(),
        },
        record: EvidenceRecord {
            // The ledger record owns its copy of the key.
            evidence_key: basis.inputs.evidence_key.clone(),
            bundle_sha256: digest,
            llm: llm_evidence(basis),
            tier: basis.tier,
        },
    }
}

/// How the LLM stage ran, as the evidence record says it: only a stage that completed with no
/// degraded role, or a tier without one, is `complete` (R3-17).
fn llm_evidence(basis: &Basis<'_>) -> LlmEvidence {
    let inputs = basis.inputs;
    match inputs.llm.effective(&inputs.pr) {
        LlmStage::Deferred { .. } => LlmEvidence::Deferred,
        LlmStage::NotRun {
            reason: NotRunReason::Draft,
        } => LlmEvidence::SkippedDraft,
        LlmStage::Completed
        | LlmStage::NotRun {
            reason: NotRunReason::NoLlmTier,
        } => {
            if basis.decision.degraded.is_empty() {
                LlmEvidence::Complete
            } else {
                LlmEvidence::Degraded
            }
        }
    }
}

/// The check update for a decision: its conclusion, or `failure persist-error` when persisting
/// failed on a trust-path or exec-surface PR (S9).
fn decided_update<L, U>(
    basis: &Basis<'_>,
    evidence: Option<&EvidenceEntry>,
    persist: Option<&PersistFailure<L, U>>,
) -> CheckUpdate {
    let (decision, classes) = (basis.decision, basis.classes);
    let fail_closed = persist.filter(|_| !classes.is_empty());
    let (conclusion, reasons) = match fail_closed {
        Some(failure) => {
            let mut reasons = decision.reasons.clone();
            reasons.push(Reason {
                code: ReasonCode::PersistError,
                detail: failure.code().to_owned(),
                fp: None,
            });
            (Conclusion::Failure, Cow::Owned(reasons))
        }
        None => (
            decision.conclusion,
            Cow::Borrowed(decision.reasons.as_slice()),
        ),
    };
    let warning = match fail_closed {
        Some(_) => None,
        None => persist.map(PersistFailure::code),
    };
    let unapplied_waivers = waivers::unapplied(basis.rubric, basis.inputs, decision);
    let summary = render::summary(&Summary {
        conclusion,
        reasons: &reasons,
        facts: Some(Facts {
            tier: basis.tier,
            classes,
            counts: decision.counts,
            degraded: &decision.degraded,
            cost: basis.cost,
            reasons_digest: decision.reasons_digest(),
            ignored_approvals: basis.ignored_approvals,
            unapplied_waivers: &unapplied_waivers,
        }),
        warning,
    });
    CheckUpdate {
        state: state_of(conclusion),
        title: render::title(conclusion, &reasons),
        summary,
        text: render::text(&Projection {
            decision_key: &decision.key,
            inputs_digest: Some(&decision.inputs_digest),
            evidence,
        }),
        // The check owns its external id; the decision keeps its key.
        external_id: decision.key.clone(),
    }
}

/// The check state a conclusion is published as: `pending` is `in_progress`, never `neutral`.
const fn state_of(conclusion: Conclusion) -> CheckState {
    match conclusion {
        Conclusion::Success => CheckState::Completed(CheckConclusion::Success),
        Conclusion::Failure => CheckState::Completed(CheckConclusion::Failure),
        Conclusion::Neutral => CheckState::Completed(CheckConclusion::Neutral),
        Conclusion::Pending => CheckState::InProgress,
    }
}

/// Posts the summary comment, or edits it when its body differs (write-if-changed), sending
/// nothing past `deadline`.
fn upsert_summary<F: VerdictForge>(
    forge: &F,
    number: std::num::NonZeroU64,
    (summary, deadline): (&str, Deadline),
) -> Result<(), (ForgeStep, F::Error)> {
    let body = render::comment(summary);
    let found = forge
        .find_summary(number, SUMMARY_MARKER, deadline)
        .map_err(|error| (ForgeStep::FindSummary, error))?;
    let written = match found {
        Some(comment) if comment.body == body => return Ok(()),
        Some(comment) => forge.update_summary(comment.id, &body, deadline),
        None => forge.create_summary(number, &body, deadline),
    };
    written.map_err(|error| (ForgeStep::WriteSummary, error))
}
