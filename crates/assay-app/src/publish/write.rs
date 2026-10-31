//! Writing the verdict: the two-phase stamp check around the per-head lock (02 §5, R3-09), the
//! bundle upload and evidence record (S9), the check and the summary comment (S8).

use std::borrow::Cow;

use assay_domain::{
    Aeu, Conclusion, Decision, DecisionInputs, LlmStage, NotRunReason, PrClass, Reason, ReasonCode,
    Rubric, Tier,
};

use super::ports::{
    CheckConclusion, CheckName, CheckRun, CheckState, CheckStatus, CheckUpdate, EvidenceEntry,
    EvidencePointer, EvidenceRecord, LlmEvidence, PublishLedger, VerdictForge,
};
use super::render::{self, Facts, Projection, SUMMARY_MARKER, Summary};
use super::waivers;
use super::{ForgeStep, PersistFailure, PublishError, Published, Stale, Target, Unlock, Written};
use crate::config::Sha256Digest;
use crate::eligibility::IgnoredApproval;

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

/// Publishes `pending` when the check is still this run's to write.
pub(super) fn write<F: VerdictForge, L: PublishLedger, U>(
    target: &Target<'_>,
    ports: Writer<'_, F, L>,
    pending: Pending<'_, L::Error, U>,
) -> WriteOutcome<F, L, U> {
    let event = target.pull_request;
    let name = CheckName::for_base(&event.base_ref, target.default_branch);
    let observed = match observe(target, ports.forge, &name, pending.key())? {
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
        .acquire(target.run_uid.repo_id, &event.head_sha)
        .map_err(PublishError::Lock)?
    else {
        return Err(PublishError::LockBusy);
    };
    let locked = under_lock(target, ports, (&name, &observed), prepared);
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

/// What the first read of the check allows.
enum Observation {
    /// The check is this run's to write: stamped for it, or carrying `key` but not completed.
    Writable(CheckRun),
    /// The check is completed under `key` already: nothing to write.
    Unchanged,
    /// The check carries neither `key` nor this run's stamp: another run owns it.
    Foreign,
}

/// Reads the App check `name` on the head and judges it against `key` and this run's stamp,
/// before any write or the lock.
fn observe<F: VerdictForge, E>(
    target: &Target<'_>,
    forge: &F,
    name: &CheckName,
    key: &str,
) -> Result<Observation, PublishError<F::Error, E>> {
    let observed = forge
        .find_check(&target.pull_request.head_sha, name)
        .map_err(PublishError::Observe)?
        .ok_or(PublishError::CheckMissing)?;
    Ok(if observed.external_id == key {
        if observed.status == CheckStatus::Completed {
            Observation::Unchanged
        } else {
            Observation::Writable(observed)
        }
    } else if is_own_stamp(&observed.external_id, target) {
        Observation::Writable(observed)
    } else {
        Observation::Foreign
    })
}

/// Whether `external_id` is `open`'s stamp for this run: `pending:<run_id>:<seq>`, with `seq`
/// one or more ASCII digits and nothing else.
fn is_own_stamp(external_id: &str, target: &Target<'_>) -> bool {
    let prefix = format!("pending:{}:", target.run_uid.run_id);
    external_id
        .strip_prefix(&prefix)
        .is_some_and(|seq| !seq.is_empty() && seq.bytes().all(|byte| byte.is_ascii_digit()))
}

/// Re-reads the check under the lock and, when it is unchanged, records the evidence and writes
/// the check and the summary comment.
fn under_lock<F: VerdictForge, L: PublishLedger, U>(
    target: &Target<'_>,
    ports: Writer<'_, F, L>,
    (name, observed): (&CheckName, &CheckRun),
    prepared: Prepared<'_, L::Error, U>,
) -> UnderLock<F::Error, L::Error, U> {
    let event = target.pull_request;
    let current = ports
        .forge
        .find_check(&event.head_sha, name)
        .map_err(|error| (ForgeStep::ReadCheck, error))?;
    if current.as_ref() != Some(observed) {
        return Ok(Locked::Stale);
    }
    let (update, persist) = match prepared {
        Prepared::Refused(update) => (update, None),
        Prepared::Decided { basis, persisted } => {
            let (entry, persist) = match persisted {
                Ok(digest) => {
                    let entry = evidence_entry(target, &basis, digest);
                    match ports.ledger.record_evidence(&entry) {
                        Ok(()) => (Some(entry), None),
                        Err(error) => (None, Some(PersistFailure::Ledger(error))),
                    }
                }
                Err(failure) => (None, Some(failure)),
            };
            let update = decided_update(&basis, entry.as_ref(), persist.as_ref());
            (update, persist)
        }
    };
    ports
        .forge
        .update_check(observed.id, &update)
        .map_err(|error| (ForgeStep::UpdateCheck, error))?;
    upsert_summary(ports.forge, target, &update.summary)?;
    Ok(Locked::Written {
        state: update.state,
        persist,
    })
}

/// The evidence pointer and record of an uploaded bundle (02 §5).
fn evidence_entry(target: &Target<'_>, basis: &Basis<'_>, digest: Sha256Digest) -> EvidenceEntry {
    let run_uid = target.run_uid;
    EvidenceEntry {
        repo_id: run_uid.repo_id,
        pr: target.pull_request.number,
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

/// Posts the summary comment, or edits it when its body differs (write-if-changed).
fn upsert_summary<F: VerdictForge>(
    forge: &F,
    target: &Target<'_>,
    summary: &str,
) -> Result<(), (ForgeStep, F::Error)> {
    let number = target.pull_request.number;
    let body = render::comment(summary);
    let found = forge
        .find_summary(number, SUMMARY_MARKER)
        .map_err(|error| (ForgeStep::FindSummary, error))?;
    let written = match found {
        Some(comment) if comment.body == body => return Ok(()),
        Some(comment) => forge.update_summary(comment.id, &body),
        None => forge.create_summary(number, &body),
    };
    written.map_err(|error| (ForgeStep::WriteSummary, error))
}
