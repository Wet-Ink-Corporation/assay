//! Doctor's warning before the OAuth token expires (05 §5, 06 F4, #93): the expiry is recorded
//! when the token is created with `claude setup-token`, and doctor warns from
//! [`EXPIRY_WARNING_DAYS`] days before it.

use std::fmt;

use crate::calendar::CalendarDate;
use crate::preflight::TOKEN_ROTATION_RUNBOOK;

/// How many days before the recorded expiry doctor starts to warn: T-30 (05 §5).
pub const EXPIRY_WARNING_DAYS: u32 = 30;

/// What doctor says about the token's recorded expiry. Each links the rotation runbook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpiryWarning {
    /// The token expires within [`EXPIRY_WARNING_DAYS`] days: rotate it now.
    ExpiresSoon {
        /// The recorded expiry.
        expires_on: CalendarDate,
        /// Days from today to the expiry, 1 to [`EXPIRY_WARNING_DAYS`].
        days_left: u32,
    },
    /// The recorded expiry is today or past: the token no longer works and the LLM stage is
    /// unavailable until it is rotated (F4).
    Expired {
        /// The recorded expiry.
        expires_on: CalendarDate,
        /// Days from the expiry to today, 0 on the day itself.
        days_ago: u32,
    },
}

impl ExpiryWarning {
    /// Whether the token is past its recorded expiry, a failure rather than a warning.
    #[must_use]
    pub const fn is_expired(&self) -> bool {
        match self {
            Self::ExpiresSoon { .. } => false,
            Self::Expired { .. } => true,
        }
    }
}

impl fmt::Display for ExpiryWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExpiresSoon {
                expires_on,
                days_left,
            } => write!(
                f,
                "the OAuth token expires on {expires_on}, in {days_left} days; \
                 rotate it: {TOKEN_ROTATION_RUNBOOK}"
            ),
            Self::Expired {
                expires_on,
                days_ago,
            } => write!(
                f,
                "the OAuth token expired on {expires_on}, {days_ago} days ago; \
                 rotate it: {TOKEN_ROTATION_RUNBOOK}"
            ),
        }
    }
}

/// Doctor's judgement of the token's recorded expiry on `today` (05 §5).
///
/// `None` while more than [`EXPIRY_WARNING_DAYS`] days remain;
/// [`ExpiryWarning::ExpiresSoon`] from that many days out to the day before;
/// [`ExpiryWarning::Expired`] from `expires_on` itself onwards, since the recorded date does
/// not say at which hour the token stops working.
#[must_use]
pub fn token_expiry_warning(
    expires_on: CalendarDate,
    today: CalendarDate,
) -> Option<ExpiryWarning> {
    let (expiry, day) = (expires_on.days_since_epoch(), today.days_since_epoch());
    match expiry.checked_sub(day) {
        Some(days_left @ 1..) if days_left <= EXPIRY_WARNING_DAYS => {
            Some(ExpiryWarning::ExpiresSoon {
                expires_on,
                days_left,
            })
        }
        Some(1..) => None,
        Some(0) | None => Some(ExpiryWarning::Expired {
            expires_on,
            days_ago: day.saturating_sub(expiry),
        }),
    }
}
