//! The `Clock` port: the wall clock and waiting, for a use case that waits (`admit` waits for a
//! review slot, 06 §1), so a test runs on a fake clock and never sleeps (TST-10).

use std::time::{Duration, SystemTime};

/// The wall clock, and a way to wait on it.
pub trait Clock {
    /// The wall-clock time now.
    fn now(&self) -> SystemTime;

    /// Returns after `duration` has passed. A fake clock moves its time forward instead.
    fn sleep(&mut self, duration: Duration);
}
