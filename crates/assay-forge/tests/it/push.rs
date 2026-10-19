//! Deleting a lease ref by compare-and-set, `git push --force-with-lease=<ref>:<expected>
//! <remote> :<ref>` (06 §1, A-33), against local bare repositories built with plain git: the
//! remote's receive-pack checks the old value, so a stale holder never deletes a newer lease.

use std::error::Error;
use std::path::{Path, PathBuf};

use assay_forge::git::{
    BranchRef, CommitSha, DeleteOutcome, GitEnvironment, GitLimits, ObjectRepo, ParseRefError,
    ParseRemoteError, PushRemote,
};
use tempfile::TempDir;

use crate::support::plain_git;

/// The empty tree every lease commit carries.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
const SLOT_0: &str = "refs/heads/slot-0";

/// A bare "ledger" repository and the object-only repository the push runs from.
struct Ledger {
    scratch: TempDir,
    pusher: ObjectRepo,
    remote: PushRemote,
}

impl Ledger {
    fn bare(&self) -> PathBuf {
        self.scratch.path().join("ledger.git")
    }

    /// A new parentless commit with `message` in the ledger, as `POST git/commits` makes one.
    fn lease_commit(&self, message: &str) -> Result<CommitSha, Box<dyn Error>> {
        let sha = plain_git(&self.bare(), &["commit-tree", EMPTY_TREE, "-m", message])?;
        Ok(sha.trim().parse()?)
    }

    /// Points `name` at `commit`, as `POST git/refs` or a rival writer does.
    fn point(&self, name: &str, commit: &CommitSha) -> Result<(), Box<dyn Error>> {
        plain_git(&self.bare(), &["update-ref", name, commit.as_str()])?;
        Ok(())
    }

    /// The commit `name` points at; `None` when it does not exist.
    fn tip(&self, name: &str) -> Result<Option<String>, Box<dyn Error>> {
        let listed = plain_git(
            &self.bare(),
            &["for-each-ref", "--format=%(objectname)", name],
        )?;
        Ok(listed.lines().next().map(str::to_owned))
    }

    fn delete_if(&self, name: &str, expected: &CommitSha) -> Result<DeleteOutcome, Box<dyn Error>> {
        let name: BranchRef = name.parse()?;
        Ok(self.pusher.delete_ref_if(&self.remote, &name, expected)?)
    }
}

fn ledger() -> Result<Ledger, Box<dyn Error>> {
    let scratch = tempfile::tempdir()?;
    plain_git(scratch.path(), &["init", "--quiet", "--bare", "ledger.git"])?;
    let pusher = ObjectRepo::init(
        scratch.path().join("pusher.git"),
        GitEnvironment::from_process_env()?,
        GitLimits::default(),
    )?;
    let remote = PushRemote::local_bare(scratch.path().join("ledger.git"))?;
    Ok(Ledger {
        scratch,
        pusher,
        remote,
    })
}

#[test]
fn a_ref_still_at_the_expected_commit_is_deleted() -> Result<(), Box<dyn Error>> {
    let ledger = ledger()?;
    let lease = ledger.lease_commit("lease one")?;
    ledger.point(SLOT_0, &lease)?;

    let outcome = ledger.delete_if(SLOT_0, &lease)?;

    assert_eq!(outcome, DeleteOutcome::Deleted);
    assert_eq!(ledger.tip(SLOT_0)?, None);
    Ok(())
}

#[test]
fn a_ref_that_moved_is_left_and_reported_stale() -> Result<(), Box<dyn Error>> {
    let ledger = ledger()?;
    let ours = ledger.lease_commit("lease one")?;
    let theirs = ledger.lease_commit("lease two")?;
    ledger.point(SLOT_0, &theirs)?;

    let outcome = ledger.delete_if(SLOT_0, &ours)?;

    assert_eq!(outcome, DeleteOutcome::Stale);
    assert_eq!(ledger.tip(SLOT_0)?, Some(theirs.to_string()));
    Ok(())
}

#[test]
fn an_absent_ref_is_stale() -> Result<(), Box<dyn Error>> {
    let ledger = ledger()?;
    let ours = ledger.lease_commit("lease one")?;

    let outcome = ledger.delete_if(SLOT_0, &ours)?;

    assert_eq!(outcome, DeleteOutcome::Stale);
    Ok(())
}

#[test]
fn a_stale_holder_cannot_delete_a_newer_holders_lease() -> Result<(), Box<dyn Error>> {
    let ledger = ledger()?;
    let stale = ledger.lease_commit("lease of the run that died")?;
    ledger.point(SLOT_0, &stale)?;
    // The next admit reaps the expired lease and takes the slot.
    assert_eq!(ledger.delete_if(SLOT_0, &stale)?, DeleteOutcome::Deleted);
    let newer = ledger.lease_commit("lease of the next run")?;
    ledger.point(SLOT_0, &newer)?;

    // The first holder's release comes late.
    let outcome = ledger.delete_if(SLOT_0, &stale)?;

    assert_eq!(outcome, DeleteOutcome::Stale);
    assert_eq!(ledger.tip(SLOT_0)?, Some(newer.to_string()));
    Ok(())
}

#[test]
fn branch_refs_are_only_plain_heads() {
    for accepted in [
        "refs/heads/slot-0",
        "refs/heads/lock-42-0123456789abcdef0123456789abcdef01234567",
        "refs/heads/retry/42/7/0123456789abcdef0123456789abcdef01234567/combined-micro",
    ] {
        assert_eq!(
            accepted
                .parse::<BranchRef>()
                .map(|name| name.as_str().to_owned()),
            Ok(accepted.to_owned())
        );
    }
    for refused in [
        "slot-0",
        "refs/tags/slot-0",
        "refs/heads/",
        "refs/heads/a//b",
        "refs/heads/a/../b",
        "refs/heads/a/",
        "refs/heads/a.lock",
        "refs/heads/.hidden",
        "refs/heads/a b",
        "refs/heads/a:b",
        "refs/heads/a~1",
        "refs/heads/-a",
        "refs/heads/a@{1}",
    ] {
        assert_eq!(
            refused.parse::<BranchRef>(),
            Err(ParseRefError),
            "{refused}"
        );
    }
}

#[test]
fn a_local_remote_must_be_an_absolute_path() {
    assert_eq!(
        PushRemote::local_bare(Path::new("ledger.git")).map(drop),
        Err(ParseRemoteError::NotAbsolute)
    );
}
