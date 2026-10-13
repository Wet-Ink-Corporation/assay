//! What the verifier stage produces: each finding's status and severity, why it stopped, what
//! it spent, and the events and `decide` outcome that follow (04 §4 step 8, 03 §6).

use assay_domain::{
    Aeu, CallFailure, Disposition, EventPayload, Severity, SupportOutcome, VerifierVerdict,
};

use super::candidate::{ClaimRef, Slot};
use super::verdict::{Judgement, VerdictRefusal};
use crate::model_runner::RateLimitObserved;

/// What the verifier made of one finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifierStatus {
    /// The verifier judged it.
    Judged(Judgement),
    /// A raised finding below `major`: not a candidate (04 §3).
    NotCandidate,
    /// The tier has no verifier (`micro`).
    NoVerifier,
    /// A candidate past the tier's calls: it keeps its severity, unverified, and counts for no
    /// verified-only rule (04 §5 rules 6-7); a carried one stays as last verified (04 §7).
    OverCap,
    /// A candidate the verifier failed on or never reached because it had failed: shown as an
    /// unverified major at most (06 §6, wi-9fd143).
    Unverified,
}

/// One finding after verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedFinding {
    /// Which raised or carried finding this is.
    pub at: ClaimRef,
    /// Its severity as the verifier received it.
    pub raised: Severity,
    /// Its severity after verification: `minor` if refuted, one level down if unclear, at most
    /// `major` if unverified, otherwise unchanged.
    pub severity: Severity,
    /// What the verifier made of it.
    pub status: VerifierStatus,
}

impl VerifiedFinding {
    /// The verifier's verdict, if it judged the finding (`decide`'s `ReviewedFinding::verdict`).
    #[must_use]
    pub const fn verdict(&self) -> Option<VerifierVerdict> {
        match &self.status {
            VerifierStatus::Judged(judgement) => Some(judgement.verdict()),
            VerifierStatus::NotCandidate
            | VerifierStatus::NoVerifier
            | VerifierStatus::OverCap
            | VerifierStatus::Unverified => None,
        }
    }
}

/// Why the verifier stopped before judging every candidate due.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifierFailure<E> {
    /// The call for the finding `at` ended without a usable verdict, or the stage budget was
    /// spent before it (`cap`).
    Call {
        /// The candidate being judged.
        at: ClaimRef,
        /// How the call ended.
        reason: CallFailure,
    },
    /// The runner could not make or supervise the call for the finding `at`; the call is
    /// charged at its budget.
    Runner {
        /// The candidate being judged.
        at: ClaimRef,
        /// Why.
        source: E,
    },
}

impl<E> VerifierFailure<E> {
    /// The degraded reason `decide` reads (04 §5 rules 8b, 8c).
    #[must_use]
    pub const fn reason(&self) -> CallFailure {
        match self {
            Self::Call { reason, .. } => *reason,
            Self::Runner { .. } => CallFailure::Unavailable,
        }
    }
}

/// The outcome of the verifier stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verification<E> {
    /// Every raised finding given, in the same order.
    pub findings: Vec<VerifiedFinding>,
    /// Every carried finding given, in the same order.
    pub carried: Vec<VerifiedFinding>,
    /// Why the stage stopped early, if it did.
    pub failure: Option<VerifierFailure<E>>,
    /// Model calls made, retries included.
    pub attempts: u32,
    /// What every attempt cost, to ledger.
    pub cost: Aeu,
    /// The rate-limit events of every attempt, in order.
    pub rate_limits: Vec<RateLimitObserved>,
    /// Why each answer refused as a verdict was refused, in order (at most two per call).
    pub refusals: Vec<VerdictRefusal>,
}

impl<E> Verification<E> {
    /// How the verifier ran, for `decide`: failed if it stopped early, completed if it judged a
    /// candidate, otherwise not needed.
    #[must_use]
    pub fn outcome(&self) -> SupportOutcome {
        if let Some(failure) = &self.failure {
            SupportOutcome::Failed {
                reason: failure.reason(),
            }
        } else if self.all().any(|f| f.verdict().is_some()) {
            SupportOutcome::Completed
        } else {
            SupportOutcome::NotNeeded
        }
    }

    /// The stage's events, carried findings first, each in order: `FindingVerified` for each
    /// judged finding, then `FindingDisposed` (`verifier-rejected-downgraded`) when a refutation
    /// lowered it; last, a `StageDegraded` for the verifier if it failed. An unclear verdict's
    /// step down has no disposition code (03 §5), so its `FindingVerified` is its only record.
    pub fn events(&self) -> impl Iterator<Item = EventPayload> + '_ {
        let verified = self.all().flat_map(|found| {
            let verdict = found.verdict();
            let judged = verdict.map(|verdict| EventPayload::FindingVerified { verdict });
            let downgraded = (verdict == Some(VerifierVerdict::Refuted)
                && found.severity != found.raised)
                .then_some(EventPayload::FindingDisposed {
                    code: Disposition::VerifierRejectedDowngraded,
                    from: Some(found.raised),
                    to: Some(found.severity),
                });
            judged.into_iter().chain(downgraded)
        });
        let degraded = self
            .failure
            .as_ref()
            .map(|failure| EventPayload::StageDegraded {
                stage: "verifier".to_owned(),
                reason: failure.reason().code().to_owned(),
            });
        verified.chain(degraded)
    }

    /// Every finding, carried first.
    fn all(&self) -> impl Iterator<Item = &VerifiedFinding> {
        self.carried.iter().chain(&self.findings)
    }

    fn get_mut(&mut self, slot: Slot) -> Option<&mut VerifiedFinding> {
        match slot {
            Slot::Raised(index) => self.findings.get_mut(index),
            Slot::Carried(index) => self.carried.get_mut(index),
        }
    }

    /// Applies `judgement` to the finding in `slot` (04 §4 step 8).
    pub(super) fn record(&mut self, slot: Slot, judgement: Judgement) {
        if let Some(found) = self.get_mut(slot) {
            found.severity = match judgement.verdict() {
                VerifierVerdict::Confirmed => found.raised,
                VerifierVerdict::Refuted => found.raised.min(Severity::Minor),
                VerifierVerdict::Unclear => one_level_down(found.raised),
            };
            found.status = VerifierStatus::Judged(judgement);
        }
    }

    /// Stops the stage on `failure`: the `unreached` candidates, the failed one included, are
    /// unverified and shown as a major at most. Candidates past the cap keep their severity.
    pub(super) fn stopped(
        mut self,
        failure: VerifierFailure<E>,
        unreached: impl IntoIterator<Item = Slot>,
    ) -> Self {
        for slot in unreached {
            if let Some(found) = self.get_mut(slot) {
                found.status = VerifierStatus::Unverified;
                found.severity = found.raised.min(Severity::Major);
            }
        }
        self.failure = Some(failure);
        self
    }
}

/// The severity one level below `severity`; `nit` stays `nit`.
const fn one_level_down(severity: Severity) -> Severity {
    match severity {
        Severity::Blocker => Severity::Major,
        Severity::Major => Severity::Minor,
        Severity::Minor | Severity::Nit => Severity::Nit,
    }
}
