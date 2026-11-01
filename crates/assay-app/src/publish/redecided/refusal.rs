//! decide-only's refusals of unusable evidence (D11, F19, F20): a forged bundle is
//! `failure gate-error:bundle-digest`, an absent one `failure bundle-expired`, each published
//! over the register's key and keyed over the repository, the evidence pointer and the reason,
//! never the decide-only run. The register it leaves names the decide-only run, which no
//! pointer names, so a later decide-only run recognises it by its key
//! ([`RedecideRefusal::keyed_over`]) and decides again; the hourly poll's repeats of the same
//! refusal are then write-if-changed.

use std::convert::Infallible;
use std::num::NonZeroU64;

use assay_domain::{ContentHash, Reason, ReasonCode};

use super::RedecideTarget;
use crate::config::Sha256Digest;
use crate::publish::ports::{Claim, EvidencePointer, PublishLedger, VerdictForge};
use crate::publish::write::{self, Claimant, Pending, Writer};
use crate::publish::{PublishError, PublishFailure, Published, refusal_update};
use crate::redecide::Absence;

/// Why decide-only refuses to re-decide from the evidence the ledger points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RedecideRefusal {
    /// The pointer or its bundle is not what assay wrote (F19): `gate-error:bundle-digest`.
    Forged,
    /// The bundle is gone (F20): `bundle-expired`, with `missing` or `expired` as its detail.
    Absent(Absence),
}

impl RedecideRefusal {
    /// Every refusal, in a fixed order.
    const ALL: [Self; 3] = [
        Self::Forged,
        Self::Absent(Absence::Missing),
        Self::Absent(Absence::Expired),
    ];

    /// Whether `key` is one [`publish_redecide_refusal`] publishes a refusal of `evidence` in
    /// repository `repo_id` under, for any reason: a register decide-only wrote itself over the
    /// evidence the ledger still points at, which a later decide-only run may write over.
    #[must_use]
    pub fn keyed_over(repo_id: NonZeroU64, evidence: &EvidencePointer, key: &Sha256Digest) -> bool {
        Self::ALL
            .into_iter()
            .any(|refusal| refusal_key(repo_id, evidence, &refusal.reason()) == key.as_str())
    }

    /// The reason the check fails with.
    #[must_use]
    pub fn reason(self) -> Reason {
        let (code, detail) = match self {
            Self::Forged => (ReasonCode::GateError, "bundle-digest"),
            Self::Absent(Absence::Missing) => (ReasonCode::BundleExpired, "missing"),
            Self::Absent(Absence::Expired) => (ReasonCode::BundleExpired, "expired"),
        };
        Reason {
            code,
            detail: detail.to_owned(),
            fp: None,
        }
    }
}

/// A refusal to re-decide, for the decide-only run and PR of `target`, over `claim`.
#[derive(Debug, Clone, Copy)]
pub struct UnusableEvidence<'a> {
    /// The run and the PR it publishes for.
    pub target: RedecideTarget<'a>,
    /// What decide-only read of the PR's state: the register it writes over and the evidence
    /// pointer it found unusable.
    pub claim: &'a Claim,
    /// Why the evidence is unusable.
    pub refusal: RedecideRefusal,
}

/// Publishes `unusable`'s refusal through `forge` and `ledger`.
///
/// It writes the `failure` check with the refusal's one reason, keyed `sha256` over the
/// repository, the claim's evidence pointer and the reason, only while the check carries the
/// claim's key; under the lock the register alone is written first, over the claim, and a
/// ledger that no longer holds the claim yields (`Stale::Moved`). The same refusal of the same
/// evidence is [`Published::Unchanged`] once written, whichever decide-only run repeats it.
///
/// # Errors
///
/// As [`publish`](crate::publish): there is no check to write, the lock cannot be taken, or a
/// forge call fails; [`PublishError::ClaimHead`] for a claim of another head; and
/// [`PublishError::NotFirstAttempt`] for a re-run, before any port call. A stale writer
/// yields ([`Published::Yielded`]); that is not an error.
pub fn publish_redecide_refusal<F: VerdictForge, L: PublishLedger>(
    unusable: &UnusableEvidence<'_>,
    forge: &F,
    ledger: &L,
) -> Result<Published<L::Error, Infallible>, PublishFailure<F, L>> {
    let (target, claim) = (&unusable.target, unusable.claim);
    if !target.is_first_attempt() {
        return Err(PublishError::NotFirstAttempt);
    }
    let reason = unusable.refusal.reason();
    let update = refusal_update(
        refusal_key(target.run_uid.repo_id, &claim.evidence, &reason),
        &reason,
    );
    write::write_at(
        &target.site(),
        Claimant::Register(claim),
        Writer { forge, ledger },
        Pending::<'_, L::Error, Infallible>::Refused(update),
    )
}

/// The key a decide-only refusal is published under, `sha256:<hex>` over the repository, the
/// evidence pointer and the reason; [`RedecideRefusal::keyed_over`] recognises it.
fn refusal_key(repo_id: NonZeroU64, evidence: &EvidencePointer, reason: &Reason) -> String {
    let repository = repo_id.to_string();
    let run_id = evidence.run_id.to_string();
    let attempt = evidence.run_attempt.to_string();
    ContentHash::of_files([
        ("refusal", reason.code.code().as_bytes()),
        ("detail", reason.detail.as_bytes()),
        ("repo_id", repository.as_bytes()),
        ("evidence_run_id", run_id.as_bytes()),
        ("evidence_run_attempt", attempt.as_bytes()),
        ("evidence_sha256", evidence.sha256.as_str().as_bytes()),
    ])
    .to_string()
}
