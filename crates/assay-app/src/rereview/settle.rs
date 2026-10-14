//! What becomes of each carried finding once the verifier has run
//! (`docs/architecture/04-pipeline.md` §6, §7).

use std::collections::BTreeSet;

use assay_domain::{Fingerprint, Severity, VerifierVerdict};

use super::carried::{CarriedAnchor, CarriedFinding};
use crate::publish::resolution_note;
use crate::validation::ValidFinding;
use crate::verify::{ClaimRef, Verification, VerifiedFinding, VerifierStatus};

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
    /// Refuted as carried, but this run raised the same fingerprint again and did not refute
    /// it there: the fresh finding stands, so the thread is not resolved. The outcome's
    /// `severity` and `verdict` are the refuted carried claim's (`minor`, `refuted`): they
    /// describe that claim only, so the carry set holds the fresh finding instead
    /// ([`next_carry`](super::next_carry)) and what is shown is the fresh finding's.
    Raised,
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

/// Settles each of `carried` from `verification`, `verify`'s result for them and for `raised`,
/// this run's validated findings, in order.
///
/// Only a `refuted` verdict resolves a carried finding (04 §6, §7), and never one whose
/// fingerprint this run raised again without refuting it there ([`CarriedState::Raised`]);
/// `micro` never resolves. Results are paired by the claim they name: when `verification` does
/// not hold exactly one entry per carried finding, each naming its own position, nothing is
/// settled from it and every carried finding is [`NotReverified::VerifierFailed`].
#[must_use]
pub fn settle_carried<E>(
    carried: &[CarriedFinding],
    raised: &[ValidFinding],
    verification: &Verification<E>,
) -> Vec<CarriedOutcome> {
    let paired = verification.carried.len() == carried.len()
        && verification
            .carried
            .iter()
            .enumerate()
            .all(|(index, verified)| verified.at == ClaimRef::Carried(index));
    let standing = standing(raised, verification);
    carried
        .iter()
        .enumerate()
        .map(|(index, finding)| (finding, verification.carried.get(index).filter(|_| paired)))
        .map(|(finding, verified)| match verified {
            Some(verified) => {
                let mut outcome = settle(finding, verified);
                if matches!(outcome.state, CarriedState::Resolved { .. })
                    && standing.contains(&finding.fp())
                {
                    outcome.state = CarriedState::Raised;
                }
                outcome
            }
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

/// The fingerprints this run raised and did not refute: every raised finding except one whose
/// own result, found at its position and naming it, is `refuted`. Linear in `raised`, then
/// one set lookup per carried finding.
fn standing<E>(raised: &[ValidFinding], verification: &Verification<E>) -> BTreeSet<Fingerprint> {
    raised
        .iter()
        .enumerate()
        .filter(|(index, found)| {
            verification
                .findings
                .get(*index)
                .filter(|verified| verified.at == ClaimRef::Raised(found.at))
                .and_then(VerifiedFinding::verdict)
                != Some(VerifierVerdict::Refuted)
        })
        .map(|(_, found)| found.fingerprint)
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
