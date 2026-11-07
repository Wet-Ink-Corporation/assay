//! Publishing a persisted refusal (#60 ST-14, D5 A): `assay decide` refuses a run in one
//! process and `assay publish` publishes the refusal in another, so what crosses between them
//! is the refusal's reason, as [`RefusalReason`], and [`publish_refusal`] publishes it exactly as
//! [`publish`](super::publish) publishes the [`Refusal`] itself.

use std::convert::Infallible;

use review_domain::{Reason, ReasonCode};
use thiserror::Error;

use super::ports::{PublishLedger, VerdictForge};
use super::write::{self, Pending, Writer};
use super::{PublishFailure, Published, Target};
use crate::decide::Refusal;

/// The longest refusal detail read back, in bytes (H-13); every detail a [`Refusal`] gives
/// names a job, a file, a plugin or a field, far shorter.
pub const MAX_REFUSAL_DETAIL_BYTES: usize = 1024;

/// The reason a [`Refusal`] fails a run with: `gate-error` or `plan-mismatch`, a non-empty
/// detail of at most [`MAX_REFUSAL_DETAIL_BYTES`] without control characters, and no
/// fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusalReason(Reason);

impl RefusalReason {
    /// The reason.
    #[must_use]
    pub const fn reason(&self) -> &Reason {
        &self.0
    }
}

impl From<&Refusal> for RefusalReason {
    /// [`Refusal::reason`], its detail made plain: each control character replaced with
    /// U+FFFD, cut to at most [`MAX_REFUSAL_DETAIL_BYTES`] at a character boundary, and
    /// `unnamed` when nothing is left. A detail that is already plain is kept as given,
    /// so the conversion is total and always reads back through `TryFrom<Reason>`.
    fn from(refusal: &Refusal) -> Self {
        let mut reason = refusal.reason();
        reason.detail = plain_detail(&reason.detail);
        Self(reason)
    }
}

/// The detail a refusal with an empty one is published with.
const EMPTY_DETAIL: &str = "unnamed";

/// `detail` without control characters, at most [`MAX_REFUSAL_DETAIL_BYTES`], never empty.
fn plain_detail(detail: &str) -> String {
    let mut plain = String::with_capacity(detail.len().min(MAX_REFUSAL_DETAIL_BYTES));
    for character in detail.chars() {
        let character = if character.is_control() {
            char::REPLACEMENT_CHARACTER
        } else {
            character
        };
        if plain.len().saturating_add(character.len_utf8()) > MAX_REFUSAL_DETAIL_BYTES {
            break;
        }
        plain.push(character);
    }
    if plain.is_empty() {
        EMPTY_DETAIL.to_owned()
    } else {
        plain
    }
}

/// A reason no [`Refusal`] gives, handed back.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("the reason is not one a refusal gives")]
pub struct NotARefusal {
    /// The reason refused.
    pub reason: Reason,
}

impl TryFrom<Reason> for RefusalReason {
    type Error = NotARefusal;

    /// `reason`, when a [`Refusal`] could have given it.
    fn try_from(reason: Reason) -> Result<Self, NotARefusal> {
        let code = matches!(
            reason.code,
            ReasonCode::GateError | ReasonCode::PlanMismatch
        );
        let detail = &reason.detail;
        let plain = !detail.is_empty()
            && detail.len() <= MAX_REFUSAL_DETAIL_BYTES
            && !detail.chars().any(char::is_control);
        if code && plain && reason.fp.is_none() {
            Ok(Self(reason))
        } else {
            Err(NotARefusal { reason })
        }
    }
}

/// Publishes `refusal` for `target` through `forge` and `ledger`.
///
/// It writes the `failure` check with the refusal's reason, keyed by the run and the reason,
/// as [`publish`](super::publish) publishes a [`Verdict::Refused`](super::Verdict::Refused).
/// Nothing is persisted.
///
/// # Errors
///
/// As [`publish`](super::publish): there is no check to write, the lock cannot be taken, or a
/// forge call fails. A stale writer yields ([`Published::Yielded`]); that is not an error.
pub fn publish_refusal<F: VerdictForge, L: PublishLedger>(
    target: &Target<'_>,
    refusal: &RefusalReason,
    forge: &F,
    ledger: &L,
) -> Result<Published<L::Error, Infallible>, PublishFailure<F, L>> {
    let update = super::refused(target, &refusal.0);
    write::write(
        target,
        Writer { forge, ledger },
        Pending::<'_, L::Error, Infallible>::Refused(update),
    )
}
