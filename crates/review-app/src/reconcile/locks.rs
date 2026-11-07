//! The publish locks the ledger-side reconcile sweeps: listed by name from the ledger repository,
//! each name read back into the lock it must be before its lease is read (T23, wi-b5affa).

use std::num::NonZeroU64;

use crate::config::CommitId;
use crate::ledger::{LeaseRef, LedgerRefs};

/// The most publish locks one sweep reads. Each lives 2 minutes and is released by its writer, so
/// more than this many at once means something is wrong; the sweep is refused rather than cut.
pub const MAX_PUBLISH_LOCKS: usize = 256;

/// The ledger repository's refs, with the listing of its publish locks.
pub trait LockListing: LedgerRefs {
    /// The names of the refs under `refs/heads/lock-`, at most `limit` of them: the adapter stops
    /// once it has `limit`. The reconcile passes one more than it accepts, so a cut list is
    /// refused, never read as complete. Every name is untrusted text.
    ///
    /// # Errors
    ///
    /// [`LedgerRefs::Error`] when the refs cannot be listed.
    fn publish_locks(&self, limit: usize) -> Result<Vec<String>, Self::Error>;
}

/// The repository id and head a listed lock ref names, when it is exactly the name assay gives
/// that lock (`refs/heads/lock-<repo_id>-<head_sha>`, no leading zero, 40 lowercase hex digits).
pub(super) fn lock_key(name: &str) -> Option<(NonZeroU64, CommitId)> {
    let rest = name.strip_prefix("refs/heads/lock-")?;
    let (repo_id, head) = rest.split_once('-')?;
    let repo_id: NonZeroU64 = repo_id.parse().ok()?;
    let head: CommitId = head.parse().ok()?;
    (LeaseRef::publish_lock(repo_id, &head).as_str() == name).then_some((repo_id, head))
}

#[cfg(test)]
mod tests {
    use super::lock_key;

    #[test]
    fn only_the_name_assay_writes_is_a_lock() {
        let head = "b".repeat(40);
        let key = lock_key(&format!("refs/heads/lock-42-{head}"));
        assert_eq!(
            key.map(|(repo_id, head)| (repo_id.get(), head.to_string())),
            Some((42, head.clone()))
        );
        for name in [
            format!("refs/heads/lock-042-{head}"),
            format!("refs/heads/lock-+42-{head}"),
            format!("refs/heads/lock-0-{head}"),
            format!("refs/heads/lock-42-{}", "B".repeat(40)),
            format!("refs/heads/lock-42-{head}0"),
            format!("refs/heads/lock-42{head}"),
            format!("refs/heads/slot-42-{head}"),
        ] {
            assert_eq!(lock_key(&name), None, "{name}");
        }
    }
}
