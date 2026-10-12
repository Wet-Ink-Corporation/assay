//! The process-group kill that the composition root lends to `ClaudeHeadless` (wi-603c63).

use std::io;

/// Kills a process group: how [`ClaudeHeadless`](super::ClaudeHeadless) stops the CLI together
/// with every process the CLI started (H-13).
///
/// The production implementation is `assay-forge`'s group killer, which this crate may not
/// depend on (ADR-07), so `assay-cli` lends it here (wi-603c63).
pub trait GroupKill {
    /// Kills every live member of process group `group` and returns once none is left.
    ///
    /// `group` is the pid of a child that leads its own process group. An implementation
    /// signals nothing when no member is alive, because once the group is empty and its leader
    /// reaped the id may name an unrelated process.
    ///
    /// # Errors
    ///
    /// When the group could not be observed or signalled, or still has a live member when the
    /// implementation stops waiting: the group may then still be running.
    fn kill_group(&self, group: u32) -> io::Result<()>;

    /// Kills process group `group`, whose leader (the child with pid `group`) has not been
    /// reaped yet, and returns once no live member is left.
    ///
    /// The unreaped leader holds the id, so it cannot have been reused and the group is
    /// signalled without first looking for a live member.
    ///
    /// # Errors
    ///
    /// When the group could not be signalled or observed, or still has a live member when the
    /// implementation stops waiting: the group may then still be running.
    fn kill_led_group(&self, group: u32) -> io::Result<()>;
}

impl<K: GroupKill + ?Sized> GroupKill for &K {
    fn kill_group(&self, group: u32) -> io::Result<()> {
        (**self).kill_group(group)
    }

    fn kill_led_group(&self, group: u32) -> io::Result<()> {
        (**self).kill_led_group(group)
    }
}
