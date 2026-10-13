//! What the verifier stage produces: each finding's status and severity, why it stopped, what
//! it spent, and the events and `decide` outcome that follow (04 §4 step 8, 03 §6).

use assay_domain::{
    Aeu, CallFailure, Disposition, EventPayload, Severity, SupportOutcome, VerifierVerdict,
};

use super::verdict::Judgement;
use crate::model_runner::RateLimitObserved;
use crate::validation::FindingRef;

/// What the verifier made of one finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifierStatus {
    /// The verifier judged it.
    Judged(Judgement),
    /// Below `major`: not a candidate (04 §3).
    NotCandidate,
    /// The tier has no verifier (`micro`).
    NoVerifier,
    /// A candidate past the tier's calls; it counts for no verified-only rule (04 §5 rules 6-7).
    OverCap,
    /// A candidate the verifier failed on or never reached because it had failed: shown as an
    /// unverified major at most (06 §6, wi-9fd143).
    Unverified,
}

/// One finding after verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedFinding {
    /// Which validated finding this is.
    pub at: FindingRef,
    /// Its severity after validation, as the verifier received it.
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
    /// The call for the finding `at` ended without a usable verdict.
    Call {
        /// The candidate being judged.
        at: FindingRef,
        /// How the call ended.
        reason: CallFailure,
    },
    /// The runner could not make or supervise the call for the finding `at`; the call is
    /// charged at its cap.
    Runner {
        /// The candidate being judged.
        at: FindingRef,
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
    /// Every finding given, in the same order.
    pub findings: Vec<VerifiedFinding>,
    /// Why the stage stopped early, if it did.
    pub failure: Option<VerifierFailure<E>>,
    /// Model calls made, retries included.
    pub attempts: u32,
    /// What every attempt cost, to ledger.
    pub cost: Aeu,
    /// The rate-limit events of every attempt, in order.
    pub rate_limits: Vec<RateLimitObserved>,
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
        } else if self.findings.iter().any(|f| f.verdict().is_some()) {
            SupportOutcome::Completed
        } else {
            SupportOutcome::NotNeeded
        }
    }

    /// The stage's events, in finding order: `FindingVerified` for each judged finding, then
    /// `FindingDisposed` (`verifier-rejected-downgraded`) when a refutation lowered it; last, a
    /// `StageDegraded` for the verifier if it failed. An unclear verdict's step down has no
    /// disposition code (03 §5), so its `FindingVerified` is its only record.
    pub fn events(&self) -> impl Iterator<Item = EventPayload> + '_ {
        let verified = self.findings.iter().flat_map(|found| {
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

    /// Applies `judgement` to the finding at `index` (04 §4 step 8).
    pub(super) fn record(&mut self, index: usize, judgement: Judgement) {
        if let Some(found) = self.findings.get_mut(index) {
            found.severity = match judgement.verdict() {
                VerifierVerdict::Confirmed => found.raised,
                VerifierVerdict::Refuted => found.raised.min(Severity::Minor),
                VerifierVerdict::Unclear => one_level_down(found.raised),
            };
            found.status = VerifierStatus::Judged(judgement);
        }
    }

    /// Stops the stage on `failure`: every candidate not yet judged is unverified, shown as a
    /// major at most.
    pub(super) fn stopped(mut self, failure: VerifierFailure<E>) -> Self {
        for found in &mut self.findings {
            if matches!(found.status, VerifierStatus::OverCap) {
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
