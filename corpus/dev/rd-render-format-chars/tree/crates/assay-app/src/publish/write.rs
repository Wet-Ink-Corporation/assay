//! Writing the verdict: the two-phase stamp check around the per-head lock (02 §5, R3-09), the
//! bundle upload and evidence record (S9), the check and the summary comment (S8).

use std::borrow::Cow;

use assay_domain::{Conclusion, Decision, LlmStage, NotRunReason, Reason, ReasonCode};

use super::bundle::{self, BUNDLE_FILE, Contents};
use super::ports::{
    BundleStore, CheckConclusion, CheckName, CheckRun, CheckState, CheckStatus, CheckUpdate,
    EvidenceEntry, EvidencePointer, EvidenceRecord, LlmEvidence, PublishLedger, VerdictForge,
};
use super::render::{self, Facts, Projection, SUMMARY_MARKER, Summary};
use super::{
    Decided, ForgeStep, PersistFailure, PublishError, PublishOutcome, PublishPorts, Published,
    Stale, Target, Unlock, Written,
};
use crate::config::Sha256Digest;

/// What one writer publishes.
pub(super) enum Pending<'w> {
    /// A refusal, whose update is ready.
    Refused(CheckUpdate),
    /// A decision, whose update depends on whether persisting it succeeds.
    Decided {
        decided: &'w Decided<'w>,
        decision: &'w Decision,
    },
}

impl Pending<'_> {
    /// The key the check carries once this is published.
    fn key(&self) -> &str {
        match self {
            Self::Refused(update) => &update.external_id,
            Self::Decided { decision, .. } => &decision.key,
        }
    }
}

/// A pending publication with its bundle uploaded, or why it was not.
enum Prepared<'w, L, B> {
    Refused(CheckUpdate),
    Decided {
        decided: &'w Decided<'w>,
        decision: &'w Decision,
        uploaded: Result<Sha256Digest, PersistFailure<L, B>>,
    },
}

/// What happened under the lock.
enum Locked<L, B> {
    Stale,
    Written {
        state: CheckState,
        persist: Option<PersistFailure<L, B>>,
    },
}

/// What happened under the lock, or the forge call that failed there.
type UnderLock<F, L, B> = Result<Locked<L, B>, (ForgeStep, F)>;

/// Publishes `pending` when the check is still this run's to write.
pub(super) fn write<F: VerdictForge, L: PublishLedger, B: BundleStore>(
    target: &Target<'_>,
    ports: &PublishPorts<'_, F, L, B>,
    pending: Pending<'_>,
) -> PublishOutcome<F, L, B> {
    let event = target.pull_request;
    let name = CheckName::for_base(&event.base_ref, target.default_branch);
    let observed = ports
        .forge
        .find_check(&event.head_sha, &name)
        .map_err(PublishError::Observe)?
        .ok_or(PublishError::CheckMissing)?;
    let key = pending.key();
    if observed.external_id == key && observed.status == CheckStatus::Completed {
        return Ok(Published::Unchanged);
    }
    if observed.external_id != key && !is_own_stamp(&observed.external_id, target) {
        return Ok(Published::Yielded {
            stale: Stale::Foreign,
            unlock: Unlock::NotTaken,
        });
    }
    // Uploaded before the lock: it is slow, and an artifact nothing points to is harmless.
    let prepared = match pending {
        Pending::Refused(update) => Prepared::Refused(update),
        Pending::Decided { decided, decision } => Prepared::Decided {
            decided,
            decision,
            uploaded: upload(target, decided, decision, ports),
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

/// Whether `external_id` is `open`'s stamp for this run: `pending:<run_id>:<seq>`, with `seq`
/// one or more ASCII digits and nothing else.
fn is_own_stamp(external_id: &str, target: &Target<'_>) -> bool {
    let prefix = format!("pending:{}:", target.run_uid.run_id);
    external_id
        .strip_prefix(&prefix)
        .is_some_and(|seq| !seq.is_empty() && seq.bytes().all(|byte| byte.is_ascii_digit()))
}

/// Builds and uploads the bundle; its digest when both succeed.
fn upload<F, L: PublishLedger, B: BundleStore>(
    target: &Target<'_>,
    decided: &Decided<'_>,
    decision: &Decision,
    ports: &PublishPorts<'_, F, L, B>,
) -> Result<Sha256Digest, PersistFailure<L::Error, B::Error>> {
    let bytes = bundle::bundle(&Contents {
        run_uid: target.run_uid,
        run: decided.run,
        inputs: decided.inputs,
        decision,
        events: decided.events,
    })
    .map_err(PersistFailure::Bundle)?;
    let artifact = &decided.run.record().bundle_artifact;
    ports
        .bundles
        .upload(artifact, BUNDLE_FILE, &bytes)
        .map_err(PersistFailure::Upload)?;
    Ok(Sha256Digest::of(&bytes))
}

/// Re-reads the check under the lock and, when it is unchanged, records the evidence and writes
/// the check and the summary comment.
fn under_lock<F: VerdictForge, L: PublishLedger, B: BundleStore>(
    target: &Target<'_>,
    ports: &PublishPorts<'_, F, L, B>,
    (name, observed): (&CheckName, &CheckRun),
    prepared: Prepared<'_, L::Error, B::Error>,
) -> UnderLock<F::Error, L::Error, B::Error> {
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
        Prepared::Decided {
            decided,
            decision,
            uploaded,
        } => {
            let (entry, persist) = match uploaded {
                Ok(digest) => {
                    let entry = evidence_entry(target, decided, decision, digest);
                    match ports.ledger.record_evidence(&entry) {
                        Ok(()) => (Some(entry), None),
                        Err(error) => (None, Some(PersistFailure::Ledger(error))),
                    }
                }
                Err(failure) => (None, Some(failure)),
            };
            let update = decided_update(decided, decision, entry.as_ref(), persist.as_ref());
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
fn evidence_entry(
    target: &Target<'_>,
    decided: &Decided<'_>,
    decision: &Decision,
    digest: Sha256Digest,
) -> EvidenceEntry {
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
            evidence_key: decided.inputs.evidence_key.clone(),
            bundle_sha256: digest,
            llm: llm_evidence(decided, decision),
            tier: decided.run.tier(),
        },
    }
}

/// How the LLM stage ran, as the evidence record says it: only a stage that completed with no
/// degraded role, or a tier without one, is `complete` (R3-17).
fn llm_evidence(decided: &Decided<'_>, decision: &Decision) -> LlmEvidence {
    let inputs = decided.inputs;
    match inputs.llm.effective(&inputs.pr) {
        LlmStage::Deferred { .. } => LlmEvidence::Deferred,
        LlmStage::NotRun {
            reason: NotRunReason::Draft,
        } => LlmEvidence::SkippedDraft,
        LlmStage::Completed
        | LlmStage::NotRun {
            reason: NotRunReason::NoLlmTier,
        } => {
            if decision.degraded.is_empty() {
                LlmEvidence::Complete
            } else {
                LlmEvidence::Degraded
            }
        }
    }
}

/// The check update for a decision: its conclusion, or `failure persist-error` when persisting
/// failed on a trust-path or exec-surface PR (S9).
fn decided_update<L, B>(
    decided: &Decided<'_>,
    decision: &Decision,
    evidence: Option<&EvidenceEntry>,
    persist: Option<&PersistFailure<L, B>>,
) -> CheckUpdate {
    let classes = &decided.run.tiering().classes;
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
    let summary = render::summary(&Summary {
        conclusion,
        reasons: &reasons,
        facts: Some(Facts {
            tier: decided.run.tier(),
            classes,
            counts: decision.counts,
            degraded: &decision.degraded,
            cost: decided.cost,
            reasons_digest: decision.reasons_digest(),
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
