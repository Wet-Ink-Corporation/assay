//! What becomes of each carried finding once the verifier has run
//! (`docs/architecture/04-pipeline.md` §6, §7).

use assay_domain::{Fingerprint, Severity, VerifierVerdict};

use super::carried::{CarriedAnchor, CarriedFinding};
use crate::publish::resolution_note;
use crate::verify::{Verification, VerifiedFinding, VerifierStatus};

/// A carried finding after this run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarriedOutcome {
    /// Its fingerprint, naming its thread.
    pub fp: Fingerprint,
    /// Its severity as last published.
    pub raised: Severity,
    /// Its severity after this run: `minor` once refuted (04 §4 step 8), one level down if
    /// unclear, at most `major` if the verifier failed, otherwise as last published.
    pub severity: Severity,
    /// This run's verdict on it; `None` unless it was re-verified, so rules 6 and 7, which read
    /// only confirmed findings, never count one that was not.
    pub verdict: Option<VerifierVerdict>,
    /// What happens to it and its thread.
    pub state: CarriedState,
}

/// What happens to a carried finding and its thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CarriedState {
    /// Refuted at head: the defect is gone, so its thread is resolved with `note`. The finding
    /// is still recorded, downgraded to `minor` (`verifier-rejected-downgraded`), never dropped.
    Resolved {
        /// The note posted on the thread, the verifier's reason made inert.
        note: String,
    },
    /// Confirmed or unclear at head: it stays open at its new severity.
    Open,
    /// Not re-verified this run: it stays open as of its last verification and its thread is
    /// left open.
    NotReverified(NotReverified),
}

/// Why a carried finding was not re-verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NotReverified {
    /// Past the tier's verifier cap: listed "not re-verified (cap)", excluded from rules 6-7.
    Cap,
    /// The verifier stage failed before judging it.
    VerifierFailed,
    /// `micro` (no verifier): its evidence is still at head, so it stays open and advisory.
    AnchorPresent,
    /// `micro` (no verifier): its evidence is not at head (or was not looked for), so it needs
    /// re-verification; that is not resolution.
    AnchorGone,
}

impl NotReverified {
    /// How the summary lists it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Cap => "not re-verified (cap)",
            Self::VerifierFailed => "not re-verified (verifier failed)",
            Self::AnchorPresent => "not re-verified (micro): still present at head",
            Self::AnchorGone => "needs re-verification",
        }
    }
}

/// Settles each of `carried` from `verification`, `verify`'s result for them, in order.
///
/// Only a `refuted` verdict resolves a carried finding (04 §6, §7); `micro` never does. A
/// carried finding `verification` has no entry for is [`NotReverified::VerifierFailed`], as is
/// one the verifier failed on.
#[must_use]
pub fn settle_carried<E>(
    carried: &[CarriedFinding],
    verification: &Verification<E>,
) -> Vec<CarriedOutcome> {
    carried
        .iter()
        .enumerate()
        .map(|(index, finding)| match verification.carried.get(index) {
            Some(verified) => settle(finding, verified),
            None => CarriedOutcome {
                fp: finding.fp(),
                raised: finding.severity(),
                severity: finding.severity(),
                verdict: None,
                state: CarriedState::NotReverified(NotReverified::VerifierFailed),
            },
        })
        .collect()
}

/// One carried finding's outcome from its verifier result.
fn settle(finding: &CarriedFinding, verified: &VerifiedFinding) -> CarriedOutcome {
    let state = match &verified.status {
        VerifierStatus::Judged(judgement) => match judgement.verdict() {
            VerifierVerdict::Refuted => CarriedState::Resolved {
                note: resolution_note(judgement.reason()),
            },
            VerifierVerdict::Confirmed | VerifierVerdict::Unclear => CarriedState::Open,
        },
        VerifierStatus::OverCap => CarriedState::NotReverified(NotReverified::Cap),
        VerifierStatus::Unverified => CarriedState::NotReverified(NotReverified::VerifierFailed),
        VerifierStatus::NoVerifier => CarriedState::NotReverified(match finding.anchor() {
            CarriedAnchor::AtHead => NotReverified::AnchorPresent,
            CarriedAnchor::Gone | CarriedAnchor::Unchecked => NotReverified::AnchorGone,
        }),
        // Every carried finding is a candidate; read anything else as needing re-verification.
        VerifierStatus::NotCandidate => CarriedState::NotReverified(NotReverified::AnchorGone),
    };
    CarriedOutcome {
        fp: finding.fp(),
        raised: verified.raised,
        severity: verified.severity,
        verdict: verified.verdict(),
        state,
    }
}
