//! Who may write the check, and what the ledger records under the lock before it: the evidence
//! pointer and record of a pipeline decision, or decide-only's decision pointer, and the
//! publication register of every write (D2 of #61, R3-08, R3-09).

use std::num::NonZeroU64;

use assay_domain::RunUid;

use super::{Basis, Prepared, decided_update, evidence_entry};
use crate::config::{CommitId, Sha256Digest};
use crate::ledger::{DecisionPointer, PublicationRegister};
use crate::publish::ports::{
    CheckUpdate, Claim, Claimed, DecisionEntry, PublishLedger, PublishedEntry,
};
use crate::publish::{PersistFailure, Target};

/// Where a verdict is written: the writing run, the PR, its head and base, and the repository's
/// default branch, which names the check (R3-03).
#[derive(Debug, Clone, Copy)]
pub(in crate::publish) struct Site<'a> {
    /// The writing run (attempt 1).
    pub(in crate::publish) run_uid: RunUid,
    /// The PR number.
    pub(in crate::publish) number: NonZeroU64,
    /// The head whose check is written.
    pub(in crate::publish) head_sha: &'a CommitId,
    /// The PR's base branch.
    pub(in crate::publish) base_ref: &'a str,
    /// The repository's default branch.
    pub(in crate::publish) default_branch: &'a str,
}

impl<'a> Site<'a> {
    /// Where the pipeline run `target` writes.
    pub(in crate::publish) const fn of(target: &Target<'a>) -> Self {
        let event = target.pull_request;
        Self {
            run_uid: target.run_uid,
            number: event.number,
            head_sha: &event.head_sha,
            base_ref: event.base_ref.as_str(),
            default_branch: target.default_branch,
        }
    }
}

/// What makes the check a writer's to write.
#[derive(Debug, Clone, Copy)]
pub(in crate::publish) enum Claimant<'a> {
    /// A pipeline run: the check carries its `pending:` stamp (R3-09).
    Stamp,
    /// decide-only: the check carries the key `claim`'s register names (D2 of #61).
    Register(&'a Claim),
}

impl<'a> Claimant<'a> {
    /// decide-only's claim, if this is decide-only.
    pub(in crate::publish) const fn claim(self) -> Option<&'a Claim> {
        match self {
            Self::Stamp => None,
            Self::Register(claim) => Some(claim),
        }
    }

    /// Whether `external_id` is the claim: `open`'s stamp for the run at `site`
    /// (`pending:<run_id>:<seq>`, `seq` one or more ASCII digits and nothing else), or exactly the
    /// register's key. A `pending:` stamp is never a register's key, which is a digest.
    pub(super) fn owns(self, external_id: &str, site: &Site<'_>) -> bool {
        match self {
            Self::Stamp => {
                let prefix = format!("pending:{}:", site.run_uid.run_id);
                external_id.strip_prefix(&prefix).is_some_and(|seq| {
                    !seq.is_empty() && seq.bytes().all(|byte| byte.is_ascii_digit())
                })
            }
            Self::Register(claim) => external_id == claim.register.key.as_str(),
        }
    }
}

/// What the ledger step leaves to write.
pub(super) enum Recorded<L, U> {
    /// decide-only's claim moved in the ledger since it was read: yield, writing nothing.
    Stale,
    /// The check update, and why persisting failed, if it did.
    Ready {
        update: CheckUpdate,
        persist: Option<PersistFailure<L, U>>,
    },
}

/// Records what `prepared` publishes at `site`, under the lock `lease`, before the check.
///
/// A refusal records the register alone. A decision records its evidence pointer and record
/// (pipeline), or its decision pointer (decide-only), with the register; a decision that persisted
/// nothing records neither, so the check then carries a key no register names and decide-only
/// yields to it (fail closed). A failed ledger write is the persist failure; nothing after it is
/// written.
pub(super) fn record<L: PublishLedger, U>(
    (site, claimant, key): (&Site<'_>, Claimant<'_>, &Sha256Digest),
    (ledger, lease): (&L, &L::Lease),
    prepared: Prepared<'_, L::Error, U>,
) -> Recorded<L::Error, U> {
    let register = PublicationRegister {
        // The register owns its copies of the head and key.
        head_sha: site.head_sha.clone(),
        key: key.clone(),
        run_id: site.run_uid.run_id,
    };
    match prepared {
        Prepared::Refused(update) => {
            let entry = PublishedEntry {
                repo_id: site.run_uid.repo_id,
                pr: site.number,
                register,
                // The entry owns its copy of the claim the ledger compares.
                over: claimant.claim().cloned(),
            };
            match ledger.record_published(lease, &entry) {
                Ok(Claimed::Recorded) => Recorded::Ready {
                    update,
                    persist: None,
                },
                Ok(Claimed::Moved) => Recorded::Stale,
                Err(error) => Recorded::Ready {
                    update,
                    persist: Some(PersistFailure::Ledger(error)),
                },
            }
        }
        Prepared::Decided { basis, persisted } => match claimant {
            Claimant::Stamp => pipeline((site, register), (ledger, lease), &basis, persisted),
            Claimant::Register(claim) => {
                decide_only((site, register, claim), (ledger, lease), &basis, persisted)
            }
        },
    }
}

/// A pipeline decision: the evidence pointer and record, then the register.
fn pipeline<L: PublishLedger, U>(
    (site, register): (&Site<'_>, PublicationRegister),
    (ledger, lease): (&L, &L::Lease),
    basis: &Basis<'_>,
    persisted: Result<Sha256Digest, PersistFailure<L::Error, U>>,
) -> Recorded<L::Error, U> {
    let (entry, persist) = match persisted {
        Ok(digest) => {
            let entry = evidence_entry(site, basis, digest);
            match ledger.record_evidence(lease, &entry) {
                Ok(()) => {
                    let published = PublishedEntry {
                        repo_id: site.run_uid.repo_id,
                        pr: site.number,
                        register,
                        over: None,
                    };
                    match ledger.record_published(lease, &published) {
                        Ok(Claimed::Recorded) => (Some(entry), None),
                        // A pipeline register is unconditional; a ledger that says otherwise
                        // is not trusted with the check.
                        Ok(Claimed::Moved) => return Recorded::Stale,
                        Err(error) => (Some(entry), Some(PersistFailure::Ledger(error))),
                    }
                }
                Err(error) => (None, Some(PersistFailure::Ledger(error))),
            }
        }
        Err(failure) => (None, Some(failure)),
    };
    Recorded::Ready {
        update: decided_update(basis, entry.as_ref(), persist.as_ref()),
        persist,
    }
}

/// A decide-only decision: its decision pointer and the register in one write, over the claim.
/// Its check projects no evidence entry: the evidence pointer names the evidence bundle, whose
/// decision is not this one.
fn decide_only<L: PublishLedger, U>(
    (site, register, claim): (&Site<'_>, PublicationRegister, &Claim),
    (ledger, lease): (&L, &L::Lease),
    basis: &Basis<'_>,
    persisted: Result<Sha256Digest, PersistFailure<L::Error, U>>,
) -> Recorded<L::Error, U> {
    let persist = match persisted {
        Ok(digest) => {
            let entry = DecisionEntry {
                repo_id: site.run_uid.repo_id,
                pr: site.number,
                pointer: DecisionPointer {
                    run_id: site.run_uid.run_id,
                    sha256: digest,
                },
                register,
                // The entry owns its copy of the claim the ledger compares.
                over: claim.clone(),
            };
            match ledger.record_decision(lease, &entry) {
                Ok(Claimed::Recorded) => None,
                Ok(Claimed::Moved) => return Recorded::Stale,
                Err(error) => Some(PersistFailure::Ledger(error)),
            }
        }
        Err(failure) => Some(failure),
    };
    Recorded::Ready {
        update: decided_update(basis, None, persist.as_ref()),
        persist,
    }
}
