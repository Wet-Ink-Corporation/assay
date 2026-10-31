//! The instant a forge call made under a lease must end by: the lease's `expires_at` (#290).

use std::time::{Duration, SystemTime};

use crate::ledger::LedgerTime;

/// The instant by which a forge call made under a lease must be over: the lease's `expires_at`,
/// after which another writer may take the lock (02 §5, R3-09).
///
/// The adapter compares it with its own clock before each request and before each wait between
/// retries, and refuses both unless they end strictly before it, so a call never sends or waits
/// past the lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Deadline(LedgerTime);

impl Deadline {
    /// The deadline at `instant`, typically a lease's `expires_at`.
    #[must_use]
    pub const fn at(instant: LedgerTime) -> Self {
        Self(instant)
    }

    /// The instant itself.
    #[must_use]
    pub const fn instant(self) -> LedgerTime {
        self.0
    }

    /// Whether something that starts at `now` and may take up to `span` ends strictly before the
    /// deadline. A sum the clock cannot hold is refused (wi-b5affa).
    #[must_use]
    pub fn admits(self, now: SystemTime, span: Duration) -> bool {
        let by = SystemTime::UNIX_EPOCH.checked_add(Duration::from_millis(self.0.unix_millis()));
        match (now.checked_add(span), by) {
            (Some(end), Some(by)) => end < by,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deadline(millis: u64) -> Deadline {
        Deadline::at(LedgerTime::from_unix_millis(millis).expect("in range"))
    }

    fn instant(millis: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(millis)
    }

    #[test]
    fn a_deadline_admits_only_what_ends_strictly_before_it() {
        let by = deadline(10_000);
        let span = Duration::from_secs(3);

        assert!(by.admits(instant(6_999), span));
        assert!(
            !by.admits(instant(7_000), span),
            "ending at the deadline is past it"
        );
        assert!(!by.admits(instant(12_000), Duration::ZERO));
    }

    #[test]
    fn a_span_that_overflows_the_clock_is_refused() {
        assert!(!deadline(10_000).admits(instant(0), Duration::MAX));
    }
}
