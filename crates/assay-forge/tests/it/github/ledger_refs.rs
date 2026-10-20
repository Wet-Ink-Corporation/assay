//! The ledger's lease refs (06 §1, #71): created through the REST API (`POST git/commits`, then
//! `POST git/refs`, which fails on an existing ref, A-26) against documented response bodies
//! (`ledger_lease_*.json`, `ledger_ref_exists.json` follow GitHub's REST schema; the ledger
//! repository is App-only, so nothing was recorded), read with `GET git/ref` and `GET
//! git/commits`, and deleted only by `git push --force-with-lease` against a local bare
//! repository standing in for the ledger (A-33).

use std::error::Error;

use assay_app::{
    CommitId, LeaseRef, LedgerRefs, LockListing, MAX_LEASE_MESSAGE_BYTES, RefCreation, RefDeletion,
    RefTip, Released, Slot, release_slot,
};
use assay_forge::git::{GitEnvironment, GitLimits, ObjectRepo, PushRemote};
use assay_forge::github::{
    Access, Api, ForgeError, InstallationId, LedgerRefsError, LedgerRefsRepo, Method, Operation,
    Permission, TokenScope, ledger_push_remote,
};
use serde_json::json;
use tempfile::TempDir;

use super::contents::LEDGER_REPOSITORY;
use super::recorded::{Recorded, Waits, fixture, fixture_json, json, reply, write_bodies};
use crate::support::plain_git;

/// The commit `ledger_lease_commit.json` creates and `ledger_lease_ref.json` points at.
const LEASE_COMMIT: &str = "5c0e3f1a9d2b47e68f0a1b2c3d4e5f6a7b8c9d0e";
/// The empty tree.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

fn slot_0() -> Result<LeaseRef, &'static str> {
    Slot::new(0).map(LeaseRef::slot).ok_or("slot 0 exists")
}

/// The lease message `ledger_lease_commit.json` carries.
fn message() -> Result<String, &'static str> {
    fixture_json("ledger_lease_commit.json")
        .get("message")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or("the fixture has a message")
}

fn path(rest: &str) -> String {
    format!("repos/{LEDGER_REPOSITORY}/{rest}")
}

/// A scratch directory with an empty object repository to push from and a bare ledger.
struct Scratch {
    dir: TempDir,
    pusher: Option<ObjectRepo>,
}

impl Scratch {
    fn new() -> Result<Self, Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        plain_git(dir.path(), &["init", "--quiet", "--bare", "ledger.git"])?;
        let pusher = ObjectRepo::init(
            dir.path().join("pusher.git"),
            GitEnvironment::from_process_env()?,
            GitLimits::default(),
        )?;
        Ok(Self {
            dir,
            pusher: Some(pusher),
        })
    }

    fn bare(&self) -> std::path::PathBuf {
        self.dir.path().join("ledger.git")
    }

    /// The adapter over `transport`, pushing to the bare ledger.
    fn refs<'a>(
        &mut self,
        transport: &'a Recorded,
        waits: &'a Waits,
    ) -> Result<LedgerRefsRepo<&'a Recorded, &'a Waits>, Box<dyn Error>> {
        let pusher = self.pusher.take().ok_or("one adapter per scratch")?;
        Ok(LedgerRefsRepo::new(
            Api::new(transport, waits),
            LEDGER_REPOSITORY.parse()?,
            pusher,
            PushRemote::local_bare(self.bare())?,
        ))
    }

    /// A lease commit in the bare ledger, with `name` pointing at it.
    fn hold(&self, name: &str, message: &str) -> Result<CommitId, Box<dyn Error>> {
        let sha = plain_git(&self.bare(), &["commit-tree", EMPTY_TREE, "-m", message])?;
        plain_git(&self.bare(), &["update-ref", name, sha.trim()])?;
        Ok(sha.trim().parse()?)
    }

    fn tip(&self, name: &str) -> Result<Option<String>, Box<dyn Error>> {
        let listed = plain_git(
            &self.bare(),
            &["for-each-ref", "--format=%(objectname)", name],
        )?;
        Ok(listed.lines().next().map(str::to_owned))
    }
}

/// `POST git/commits` answered with the lease commit.
fn commit_created(recorded: Recorded) -> Recorded {
    recorded.then(
        Method::Post,
        &path("git/commits"),
        reply(201, &[], fixture("ledger_lease_commit.json")),
    )
}

#[test]
fn a_lease_ref_is_created_at_a_new_commit_carrying_the_lease() -> Result<(), Box<dyn Error>> {
    let recorded = commit_created(Recorded::default()).then(
        Method::Post,
        &path("git/refs"),
        reply(201, &[], fixture("ledger_lease_ref.json")),
    );
    let waits = Waits::default();
    let mut scratch = Scratch::new()?;

    let created = scratch
        .refs(&recorded, &waits)?
        .create(&slot_0()?, &message()?)?;

    assert_eq!(created, RefCreation::Created(LEASE_COMMIT.parse()?));
    assert_eq!(
        write_bodies(&recorded),
        [
            Some(json!({"message": message()?, "tree": EMPTY_TREE, "parents": []})),
            Some(json!({"ref": "refs/heads/slot-0", "sha": LEASE_COMMIT})),
        ]
    );
    Ok(())
}

#[test]
fn creating_a_ref_that_exists_reads_it_and_reports_it_held() -> Result<(), Box<dyn Error>> {
    let mut theirs = fixture_json("ledger_lease_ref.json");
    theirs["object"]["sha"] = json!("a".repeat(40));
    let recorded = commit_created(Recorded::default())
        .then(
            Method::Post,
            &path("git/refs"),
            reply(422, &[], fixture("ledger_ref_exists.json")),
        )
        .ok(Method::Get, &path("git/ref/heads/slot-0"), json(&theirs));
    let waits = Waits::default();
    let mut scratch = Scratch::new()?;

    let created = scratch
        .refs(&recorded, &waits)?
        .create(&slot_0()?, &message()?)?;

    assert_eq!(created, RefCreation::Exists);
    assert_eq!(recorded.unused(), 0);
    Ok(())
}

#[test]
fn a_ref_creation_that_landed_unanswered_is_created_and_not_resent() -> Result<(), Box<dyn Error>> {
    let recorded = commit_created(Recorded::default())
        .then(Method::Post, &path("git/refs"), reply(502, &[], Vec::new()))
        .ok(
            Method::Get,
            &path("git/ref/heads/slot-0"),
            fixture("ledger_lease_ref.json"),
        );
    let waits = Waits::default();
    let mut scratch = Scratch::new()?;

    let created = scratch
        .refs(&recorded, &waits)?
        .create(&slot_0()?, &message()?)?;

    assert_eq!(created, RefCreation::Created(LEASE_COMMIT.parse()?));
    assert_eq!(recorded.writes().len(), 2, "one commit, one ref creation");
    Ok(())
}

#[test]
fn a_refused_creation_with_no_ref_after_it_is_a_rejection() -> Result<(), Box<dyn Error>> {
    let recorded = commit_created(Recorded::default())
        .then(
            Method::Post,
            &path("git/refs"),
            reply(422, &[], fixture("ledger_ref_exists.json")),
        )
        .then(
            Method::Get,
            &path("git/ref/heads/slot-0"),
            reply(404, &[], br#"{"message":"Not Found"}"#.to_vec()),
        );
    let waits = Waits::default();
    let mut scratch = Scratch::new()?;

    let created = scratch
        .refs(&recorded, &waits)?
        .create(&slot_0()?, &message()?);

    assert!(
        matches!(
            created,
            Err(LedgerRefsError::Forge(ForgeError::Rejected {
                status: 422,
                operation: Operation::CreateLedgerRef
            }))
        ),
        "{created:?}"
    );
    Ok(())
}

#[test]
fn a_commit_github_wrote_otherwise_is_refused() -> Result<(), Box<dyn Error>> {
    let mut parented = fixture_json("ledger_lease_commit.json");
    parented["parents"] = json!([{"sha": "a".repeat(40)}]);
    let recorded = Recorded::default().then(
        Method::Post,
        &path("git/commits"),
        reply(201, &[], json(&parented)),
    );
    let waits = Waits::default();
    let mut scratch = Scratch::new()?;

    let created = scratch
        .refs(&recorded, &waits)?
        .create(&slot_0()?, &message()?);

    assert!(
        matches!(
            created,
            Err(LedgerRefsError::Forge(ForgeError::Malformed {
                operation: Operation::CreateLedgerRef,
                ..
            }))
        ),
        "{created:?}"
    );
    assert_eq!(recorded.writes().len(), 1, "no ref is created at it");
    Ok(())
}

#[test]
fn a_message_over_the_cap_is_never_sent() -> Result<(), Box<dyn Error>> {
    let recorded = Recorded::default();
    let waits = Waits::default();
    let mut scratch = Scratch::new()?;

    let created = scratch
        .refs(&recorded, &waits)?
        .create(&slot_0()?, &"x".repeat(MAX_LEASE_MESSAGE_BYTES + 1));

    assert!(
        matches!(created, Err(LedgerRefsError::TooLong { .. })),
        "{created:?}"
    );
    assert_eq!(recorded.sent(), []);
    Ok(())
}

#[test]
fn a_lease_ref_is_read_with_its_commit_message() -> Result<(), Box<dyn Error>> {
    let recorded = Recorded::default()
        .ok(
            Method::Get,
            &path("git/ref/heads/slot-0"),
            fixture("ledger_lease_ref.json"),
        )
        .ok(
            Method::Get,
            &path(&format!("git/commits/{LEASE_COMMIT}")),
            fixture("ledger_lease_commit.json"),
        );
    let waits = Waits::default();
    let mut scratch = Scratch::new()?;

    let tip = scratch.refs(&recorded, &waits)?.read(&slot_0()?)?;

    assert_eq!(
        tip,
        Some(RefTip {
            commit: LEASE_COMMIT.parse()?,
            message: message()?,
        })
    );
    Ok(())
}

#[test]
fn an_absent_ref_of_a_visible_repository_reads_as_none() -> Result<(), Box<dyn Error>> {
    let recorded = Recorded::default()
        .then(
            Method::Get,
            &path("git/ref/heads/slot-0"),
            reply(404, &[], br#"{"message":"Not Found"}"#.to_vec()),
        )
        .ok(
            Method::Get,
            &format!("repos/{LEDGER_REPOSITORY}"),
            fixture("ledger_repository.json"),
        );
    let waits = Waits::default();
    let mut scratch = Scratch::new()?;

    let tip = scratch.refs(&recorded, &waits)?.read(&slot_0()?)?;

    assert_eq!(tip, None);
    Ok(())
}

#[test]
fn a_404_from_a_repository_the_token_cannot_see_is_an_error_not_a_free_slot()
-> Result<(), Box<dyn Error>> {
    let not_found = || reply(404, &[], br#"{"message":"Not Found"}"#.to_vec());
    let recorded = Recorded::default()
        .then(Method::Get, &path("git/ref/heads/slot-0"), not_found())
        .then(
            Method::Get,
            &format!("repos/{LEDGER_REPOSITORY}"),
            not_found(),
        );
    let waits = Waits::default();
    let mut scratch = Scratch::new()?;

    let tip = scratch.refs(&recorded, &waits)?.read(&slot_0()?);

    assert!(
        matches!(
            tip,
            Err(LedgerRefsError::Forge(ForgeError::Rejected {
                status: 404,
                ..
            }))
        ),
        "{tip:?}"
    );
    Ok(())
}

#[test]
fn a_ref_answered_for_another_name_or_not_at_a_commit_is_refused() -> Result<(), Box<dyn Error>> {
    let mut renamed = fixture_json("ledger_lease_ref.json");
    renamed["ref"] = json!("refs/heads/slot-1");
    let mut tagged = fixture_json("ledger_lease_ref.json");
    tagged["object"]["type"] = json!("tag");
    for answer in [renamed, tagged] {
        let recorded =
            Recorded::default().ok(Method::Get, &path("git/ref/heads/slot-0"), json(&answer));
        let waits = Waits::default();
        let mut scratch = Scratch::new()?;

        let tip = scratch.refs(&recorded, &waits)?.read(&slot_0()?);

        assert!(
            matches!(
                tip,
                Err(LedgerRefsError::Forge(ForgeError::Malformed {
                    operation: Operation::ReadLedgerRef,
                    ..
                }))
            ),
            "{answer}: {tip:?}"
        );
    }
    Ok(())
}

#[test]
fn a_lease_message_over_the_cap_is_refused_on_read() -> Result<(), Box<dyn Error>> {
    let mut long = fixture_json("ledger_lease_commit.json");
    long["message"] = json!("x".repeat(MAX_LEASE_MESSAGE_BYTES + 1));
    let recorded = Recorded::default()
        .ok(
            Method::Get,
            &path("git/ref/heads/slot-0"),
            fixture("ledger_lease_ref.json"),
        )
        .ok(
            Method::Get,
            &path(&format!("git/commits/{LEASE_COMMIT}")),
            json(&long),
        );
    let waits = Waits::default();
    let mut scratch = Scratch::new()?;

    let tip = scratch.refs(&recorded, &waits)?.read(&slot_0()?);

    assert!(
        matches!(tip, Err(LedgerRefsError::TooLong { .. })),
        "{tip:?}"
    );
    Ok(())
}

#[test]
fn a_deletion_is_pushed_with_force_with_lease_and_never_sent_to_the_api()
-> Result<(), Box<dyn Error>> {
    let recorded = Recorded::default();
    let waits = Waits::default();
    let mut scratch = Scratch::new()?;
    let held = scratch.hold("refs/heads/slot-0", "lease")?;
    let refs = scratch.refs(&recorded, &waits)?;

    let deleted = refs.delete_if(&slot_0()?, &held)?;

    assert_eq!(deleted, RefDeletion::Deleted);
    assert_eq!(scratch.tip("refs/heads/slot-0")?, None);
    assert_eq!(recorded.sent(), [], "no REST DELETE /git/refs");
    Ok(())
}

#[test]
fn a_stale_holder_cannot_release_a_newer_holders_slot() -> Result<(), Box<dyn Error>> {
    let recorded = Recorded::default();
    let waits = Waits::default();
    let mut scratch = Scratch::new()?;
    // The first holder's lease expired and was reaped; the next run holds the slot.
    let stale = scratch.hold("refs/heads/slot-0", "lease of the run that died")?;
    plain_git(&scratch.bare(), &["update-ref", "-d", "refs/heads/slot-0"])?;
    let newer = scratch.hold("refs/heads/slot-0", "lease of the next run")?;
    let refs = scratch.refs(&recorded, &waits)?;
    let slot = Slot::new(0).ok_or("slot 0")?;

    let late = release_slot(&refs, slot, &stale)?;
    let own = release_slot(&refs, slot, &newer)?;

    assert_eq!((late, own), (Released::NotHeld, Released::Released));
    assert_eq!(scratch.tip("refs/heads/slot-0")?, None);
    Ok(())
}

#[test]
fn the_ledger_remote_is_its_https_url_with_the_token_kept_out_of_sight()
-> Result<(), Box<dyn Error>> {
    let recorded = Recorded::default().then(
        Method::Post,
        "app/installations/172801043/access_tokens",
        reply(201, &[], fixture("installation_token.json")),
    );
    let waits = Waits::default();
    let scope = TokenScope::new(
        ["assay"],
        [
            (Permission::Checks, Access::Write),
            (Permission::PullRequests, Access::Write),
        ],
    )?;
    let token = Api::new(&recorded, &waits)
        .mint_installation_token(InstallationId(super::recorded::id(172_801_043)), &scope)?;

    let remote = ledger_push_remote(&LEDGER_REPOSITORY.parse()?, &token)?;

    let shown = format!("{remote:?}");
    assert!(
        shown.contains("\"https://github.com/Wet-Ink-Corporation/assay-ledger.git\""),
        "{shown}"
    );
    assert!(!shown.contains(token.secret()), "{shown}");
    Ok(())
}

/// The listing of the publish locks, from GitHub's documented `matching-refs` schema.
fn locks_path() -> String {
    path("git/matching-refs/heads/lock-?per_page=100")
}

#[test]
fn the_publish_locks_are_listed_by_name() -> Result<(), Box<dyn Error>> {
    let recorded = Recorded::default().then(
        Method::Get,
        &locks_path(),
        reply(200, &[], fixture("ledger_lock_refs.json")),
    );
    let waits = Waits::default();
    let mut scratch = Scratch::new()?;

    let names = scratch.refs(&recorded, &waits)?.publish_locks(10)?;

    assert_eq!(
        names,
        ["refs/heads/lock-1413773798-70aa6d748be6f3884e85d294112f9addf83cee59"]
    );
    Ok(())
}

#[test]
fn a_listed_ref_outside_the_locks_is_refused() -> Result<(), Box<dyn Error>> {
    let mut listed = fixture_json("ledger_lock_refs.json");
    let first = listed
        .get_mut(0)
        .and_then(|found| found.get_mut("ref"))
        .ok_or("the fixture lists a ref")?;
    *first = json!("refs/heads/slot-0");
    let recorded =
        Recorded::default().then(Method::Get, &locks_path(), reply(200, &[], json(&listed)));
    let waits = Waits::default();
    let mut scratch = Scratch::new()?;

    let names = scratch.refs(&recorded, &waits)?.publish_locks(10);

    assert!(
        matches!(
            names,
            Err(LedgerRefsError::Forge(ForgeError::Malformed {
                operation: Operation::ListLedgerRefs,
                ..
            }))
        ),
        "{names:?}"
    );
    Ok(())
}
