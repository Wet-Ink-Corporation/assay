//! decide-only's publication (#61 DO-7, D2 A, D11, J5, J10): a re-decision is written only over
//! the key the publication register names, with its decision pointer and the register in one
//! compare-and-set under the per-head lock; an unconfirmed upload persists nothing and fails
//! closed on trust-path and exec-surface PRs; a refusal of unusable evidence is keyed over the
//! evidence pointer and its reason, so it is written once.

use std::assert_matches;
use std::cell::RefCell;
use std::error::Error;
use std::num::NonZeroU64;

use assay_app::{
    Absence, CheckConclusion, CheckState, CheckStatus, Claim, DecisionBundleSource, DecisionTarget,
    DecisionTrigger, EvidenceBundle, PersistFailure, PersistOutcome, PublicationRegister,
    PublishError, Published, RedecideRefusal, RedecideTarget, Redecided, RedecidedError,
    RedecidedPersisted, RedecidedPorts, RedecidedRefused, RedecisionRequest, ReviewState,
    Sha256Digest, Stale, StoredBundle, Unconfirmed, Unlock, UnusableEvidence, prepare_decision,
    publish_redecide_refusal, publish_redecided, redecision_inputs,
};
use assay_domain::{FileChange, Lines, RunUid};
use serde_json::Value;

use crate::decide_fakes::Scenario;
use crate::eligibility_fakes::{APPROVER, By, CONTRIBUTOR, FIRST, World, lists};
use crate::plan_fakes::{FakeError, FakePr, HEAD};
use crate::publish_fakes::{Call, FakeWorld};
use crate::publish_support::{HEAD_COMMIT, NAME};
use crate::redecide_fakes::{evidence_of, redecided};
use crate::replay_fakes::Stored;
use crate::support::rubric_v1;

/// The key the register names: the pipeline's verdict, which decide-only replaces.
const CLAIMED: char = 'a';

/// decide-only's run 99.
fn decide_only_run(run_id: u64) -> Result<RunUid, Box<dyn Error>> {
    Ok(RunUid {
        repo_id: NonZeroU64::new(840_213_377).ok_or("not zero")?,
        run_id: NonZeroU64::new(run_id).ok_or("not zero")?,
        run_attempt: NonZeroU64::MIN,
    })
}

fn digest(digit: char) -> Result<Sha256Digest, Box<dyn Error>> {
    Ok(format!("sha256:{}", digit.to_string().repeat(64)).parse()?)
}

/// The decision bundles decide-only uploaded: `stored` for every run and PR asked.
struct Uploaded {
    stored: Stored,
    asked: RefCell<Vec<(RunUid, NonZeroU64, usize)>>,
}

impl Uploaded {
    const fn new(stored: Stored) -> Self {
        Self {
            stored,
            asked: RefCell::new(Vec::new()),
        }
    }
}

impl DecisionBundleSource for Uploaded {
    type Error = FakeError;

    fn fetch_decision(
        &self,
        run: RunUid,
        pr: NonZeroU64,
        limit: usize,
    ) -> Result<StoredBundle, FakeError> {
        self.asked.borrow_mut().push((run, pr, limit));
        match &self.stored {
            Stored::Bytes(bytes) => Ok(StoredBundle::Found(bytes.clone())),
            Stored::Missing => Ok(StoredBundle::Missing),
            Stored::Expired => Ok(StoredBundle::Expired),
            Stored::TooLarge => Ok(StoredBundle::TooLarge),
            Stored::Fails => Err(FakeError),
        }
    }
}

/// One re-decision of a contributor's PR, its bundle prepared, and the claim decide-only read.
struct Case {
    world: World,
    evidence: EvidenceBundle,
    bundle: Vec<u8>,
    digest: Sha256Digest,
    key: Sha256Digest,
    claim: Claim,
}

impl Case {
    /// The scheduled re-decision of `scenario`'s head, unchanged since the evidence run.
    fn of(scenario: &Scenario) -> Result<Self, Box<dyn Error>> {
        Self::in_world(scenario, World::opened_at(CONTRIBUTOR, HEAD_COMMIT))
    }

    /// Like [`Case::of`], over the PR as `world` has it.
    fn in_world(scenario: &Scenario, world: World) -> Result<Self, Box<dyn Error>> {
        let evidence = evidence_of(scenario, &world, |_| Ok(()))?;
        let state = redecided(&world, &evidence, &lists())?;
        let inputs = redecision_inputs(&RedecisionRequest {
            evidence: &evidence,
            state: &state,
            requeues: 0,
        })?;
        let rubric = rubric_v1()?;
        let prepared = prepare_decision(
            &DecisionTarget {
                run_uid: decide_only_run(99)?,
                head_sha: evidence.head(),
                base_ref: evidence.base_ref(),
                trigger: DecisionTrigger::Schedule,
                at: "2026-11-01T10:00:00Z".parse()?,
            },
            &Redecided {
                evidence: &evidence,
                rubric: &rubric,
                inputs: &inputs,
                github: &state,
                command: None,
            },
        )?;
        let claim = Claim {
            register: PublicationRegister {
                // The register owns its head.
                head_sha: evidence.head().clone(),
                key: digest(CLAIMED)?,
                run_id: evidence.pointer().run_id,
            },
            // The claim owns its copy of the pointer.
            evidence: evidence.pointer().clone(),
        };
        Ok(Self {
            world,
            key: inputs.decision_key.parse()?,
            digest: prepared.digest().clone(),
            bundle: prepared.into_bytes(),
            evidence,
            claim,
        })
    }

    /// A contributor's ordinary PR.
    fn plain() -> Result<Self, Box<dyn Error>> {
        Self::of(&Scenario::new()?)
    }

    /// A PR changing a CI workflow, a trust path.
    fn trust_path() -> Result<Self, Box<dyn Error>> {
        let mut pr = FakePr::new();
        pr.changes = assay_app::ChangedFiles::Listed(vec![FileChange::new(
            ".github/workflows/ci.yml",
            Lines::Counted {
                added: 1,
                removed: 0,
            },
        )]);
        Self::of(&Scenario::of(pr)?)
    }

    /// A contributor's PR approved at minute 4 and pushed at minute 5, so the approval is
    /// ignored.
    fn approved_before_push() -> Result<Self, Box<dyn Error>> {
        let mut world = World::opened_by(CONTRIBUTOR);
        world.commit(HEAD_COMMIT, &[FIRST], By::user(CONTRIBUTOR));
        world.review(APPROVER, ReviewState::Approved, (HEAD_COMMIT, 4));
        world.push((FIRST, HEAD_COMMIT), CONTRIBUTOR, 5);
        world.head(HEAD_COMMIT);
        Self::in_world(&Scenario::new()?, world)
    }

    /// decide-only's run `run_id` on the evidence's head and base.
    fn target(&self, run_id: u64) -> Result<RedecideTarget<'_>, Box<dyn Error>> {
        Ok(RedecideTarget {
            run_uid: decide_only_run(run_id)?,
            number: self.world.number,
            head_sha: self.evidence.head(),
            base_ref: self.evidence.base_ref(),
            default_branch: "main",
        })
    }

    /// A world whose App check carries `external_id`, with the ledger state the claim was read
    /// from.
    fn checked(&self, external_id: &str) -> Result<FakeWorld, Box<dyn Error>> {
        let world = FakeWorld::with_check(HEAD, NAME, external_id)?;
        *world.register.borrow_mut() = Some(self.claim.register.clone());
        *world.pointer.borrow_mut() = Some(self.claim.evidence.clone());
        Ok(world)
    }

    /// A world whose check carries the claimed key, completed, as the pipeline left it.
    fn claimed(&self) -> Result<FakeWorld, Box<dyn Error>> {
        let world = self.checked(self.claim.register.key.as_str())?;
        complete(&world);
        Ok(world)
    }

    /// Publishes the bundle over `world`, its upload ending `upload` and the store holding
    /// `stored`.
    fn publish(
        &self,
        world: &FakeWorld,
        upload: PersistOutcome,
        stored: &Uploaded,
    ) -> Result<Outcome, Box<dyn Error>> {
        let rubric = rubric_v1()?;
        Ok(publish_redecided(
            &RedecidedPersisted {
                target: self.target(99)?,
                claim: &self.claim,
                rubric: &rubric,
                bundle: &self.bundle,
                digest: &self.digest,
                upload,
            },
            &RedecidedPorts {
                forge: world,
                ledger: world,
                bundles: stored,
            },
        ))
    }

    /// The store holding exactly the prepared bundle.
    fn stored(&self) -> Uploaded {
        Uploaded::new(Stored::Bytes(self.bundle.clone()))
    }
}

/// What `publish_redecided` returns over the fakes.
type Outcome =
    Result<Published<FakeError, Unconfirmed<FakeError>>, RedecidedError<FakeError, FakeError>>;

/// Marks the App check completed, as a published verdict is.
fn complete(world: &FakeWorld) {
    for check in world.checks.borrow_mut().iter_mut() {
        check.status = CheckStatus::Completed;
    }
}

#[test]
fn a_re_decision_is_written_over_the_register_s_key_with_its_pointer() -> Result<(), Box<dyn Error>>
{
    let case = Case::plain()?;
    let world = case.claimed()?;

    let outcome = case.publish(&world, PersistOutcome::Success, &case.stored())?;

    let Ok(Published::Written(written)) = outcome else {
        return Err(format!("expected a write, got {outcome:?}").into());
    };
    assert_matches!(written.persist, None);
    assert_matches!(written.unlock, Unlock::Released);
    let check = world.app_check(NAME)?;
    assert_eq!(check.external_id, case.key.as_str());
    assert_eq!(
        world.writes(),
        [
            Call::Acquire,
            Call::RecordDecision,
            Call::UpdateCheck,
            Call::CreateSummary,
            Call::Release,
        ],
        "J6: no evidence, slot, lease or cost record"
    );
    let decisions = world.decisions.borrow();
    let [entry] = decisions.as_slice() else {
        return Err(format!("one decision entry, got {}", decisions.len()).into());
    };
    assert_eq!(entry.pointer.run_id, decide_only_run(99)?.run_id);
    assert_eq!(entry.pointer.sha256, case.digest);
    assert_eq!(
        entry.register,
        PublicationRegister {
            head_sha: case.evidence.head().clone(),
            key: case.key.clone(),
            run_id: decide_only_run(99)?.run_id,
        }
    );
    assert_eq!(entry.over, case.claim);
    assert!(
        world.evidence.borrow().is_empty(),
        "the evidence pointer stays"
    );
    Ok(())
}

#[test]
fn the_upload_is_confirmed_from_the_run_s_decision_artifact() -> Result<(), Box<dyn Error>> {
    let case = Case::plain()?;
    let world = case.claimed()?;
    let stored = case.stored();

    case.publish(&world, PersistOutcome::Success, &stored)??;

    assert_eq!(
        *stored.asked.borrow(),
        [(
            decide_only_run(99)?,
            case.world.number,
            assay_app::MAX_DECISION_BUNDLE_BYTES
        )]
    );
    Ok(())
}

#[test]
fn a_stamp_or_any_other_key_yields_untouched() -> Result<(), Box<dyn Error>> {
    let case = Case::plain()?;
    for external_id in ["pending:7:3", "pending:99:1", &digest('c')?.to_string()] {
        let world = case.checked(external_id)?;
        let stored = case.stored();

        let outcome = case.publish(&world, PersistOutcome::Success, &stored)?;

        assert_matches!(
            outcome,
            Ok(Published::Yielded {
                stale: Stale::Foreign,
                unlock: Unlock::NotTaken
            }),
            "{external_id}"
        );
        assert_eq!(world.writes(), [], "{external_id}");
        assert_eq!(*stored.asked.borrow(), [], "{external_id}: no fetch");
    }
    Ok(())
}

#[test]
fn a_check_changed_under_the_lock_yields() -> Result<(), Box<dyn Error>> {
    let case = Case::plain()?;
    let world = case.claimed()?;
    *world.interleave.borrow_mut() = Some(Box::new(|world: &FakeWorld| {
        world.restamp("pending:120:1");
    }));

    let outcome = case.publish(&world, PersistOutcome::Success, &case.stored())?;

    assert_matches!(
        outcome,
        Ok(Published::Yielded {
            stale: Stale::Moved,
            unlock: Unlock::Released
        })
    );
    assert_eq!(world.writes(), [Call::Acquire, Call::Release]);
    Ok(())
}

#[test]
fn a_register_or_evidence_moved_since_the_claim_yields_unwritten() -> Result<(), Box<dyn Error>> {
    let case = Case::plain()?;
    let moved = [
        |world: &FakeWorld| {
            if let Some(register) = world.register.borrow_mut().as_mut() {
                register.run_id = NonZeroU64::MAX;
            }
        },
        |world: &FakeWorld| {
            if let Some(pointer) = world.pointer.borrow_mut().as_mut() {
                pointer.run_id = NonZeroU64::MAX;
            }
        },
    ];
    for (index, move_it) in moved.into_iter().enumerate() {
        let world = case.claimed()?;
        move_it(&world);

        let outcome = case.publish(&world, PersistOutcome::Success, &case.stored())?;

        assert_matches!(
            outcome,
            Ok(Published::Yielded {
                stale: Stale::Moved,
                unlock: Unlock::Released
            }),
            "{index}"
        );
        assert_eq!(
            world.writes(),
            [Call::Acquire, Call::RecordDecision, Call::Release],
            "{index}"
        );
        assert_eq!(world.app_check(NAME)?.output, None, "{index}: unwritten");
    }
    Ok(())
}

#[test]
fn an_equal_key_on_a_completed_check_is_unchanged() -> Result<(), Box<dyn Error>> {
    let case = Case::plain()?;
    let world = case.checked(case.key.as_str())?;
    complete(&world);

    let outcome = case.publish(&world, PersistOutcome::Success, &case.stored())?;

    assert_matches!(outcome, Ok(Published::Unchanged));
    assert_eq!(*world.calls.borrow(), [Call::FindCheck]);
    Ok(())
}

#[test]
fn a_claim_of_another_head_is_refused_before_any_port() -> Result<(), Box<dyn Error>> {
    let mut case = Case::plain()?;
    case.claim.register.head_sha = "c".repeat(40).parse()?;
    let world = case.claimed()?;
    world.calls.borrow_mut().clear();

    let outcome = case.publish(&world, PersistOutcome::Success, &case.stored())?;

    assert_matches!(
        outcome,
        Err(RedecidedError::Publish(PublishError::ClaimHead))
    );
    assert_eq!(*world.calls.borrow(), []);
    Ok(())
}

/// The upload outcomes and stored files that leave the decision bundle unconfirmed.
fn unconfirmed(case: &Case) -> Vec<(&'static str, PersistOutcome, Uploaded)> {
    vec![
        ("failed", PersistOutcome::Failure, case.stored()),
        ("skipped", PersistOutcome::Skipped, case.stored()),
        (
            "missing",
            PersistOutcome::Success,
            Uploaded::new(Stored::Missing),
        ),
        (
            "expired",
            PersistOutcome::Success,
            Uploaded::new(Stored::Expired),
        ),
        (
            "other bytes",
            PersistOutcome::Success,
            Uploaded::new(Stored::Bytes(b"{}".to_vec())),
        ),
    ]
}

#[test]
fn an_unconfirmed_upload_fails_a_trust_path_pr_closed_and_records_nothing()
-> Result<(), Box<dyn Error>> {
    let case = Case::trust_path()?;
    for (name, upload, stored) in unconfirmed(&case) {
        let world = case.claimed()?;

        let outcome = case.publish(&world, upload, &stored)?;

        let Ok(Published::Written(written)) = outcome else {
            return Err(format!("{name}: expected a write, got {outcome:?}").into());
        };
        assert_matches!(written.persist, Some(PersistFailure::Upload(_)), "{name}");
        assert_eq!(
            written.state,
            CheckState::Completed(CheckConclusion::Failure),
            "{name}"
        );
        let summary = world.app_check(NAME)?.output.ok_or("written")?.summary;
        assert!(
            summary.contains("persist-error: upload"),
            "{name}: {summary}"
        );
        assert_eq!(
            world.writes(),
            [
                Call::Acquire,
                Call::UpdateCheck,
                Call::CreateSummary,
                Call::Release
            ],
            "{name}: no decision pointer and no register"
        );
        assert_eq!(*world.register.borrow(), Some(case.claim.register.clone()));
    }
    Ok(())
}

#[test]
fn an_unconfirmed_upload_only_warns_on_another_pr() -> Result<(), Box<dyn Error>> {
    let case = Case::plain()?;
    for (name, upload, stored) in unconfirmed(&case) {
        let world = case.claimed()?;

        let outcome = case.publish(&world, upload, &stored)?;

        let Ok(Published::Written(written)) = outcome else {
            return Err(format!("{name}: expected a write, got {outcome:?}").into());
        };
        assert_matches!(written.persist, Some(PersistFailure::Upload(_)), "{name}");
        assert_eq!(
            written.state,
            CheckState::Completed(CheckConclusion::Success),
            "{name}"
        );
        let summary = world.app_check(NAME)?.output.ok_or("written")?.summary;
        assert!(summary.contains("(`upload`)"), "{name}: {summary}");
        assert!(world.decisions.borrow().is_empty(), "{name}");
    }
    Ok(())
}

/// Publishes `case`'s bundle for `target` over `claim`, given as hashing to `digest`, and the
/// port calls it made.
fn published_as(
    case: &Case,
    (target, claim, digest): (RedecideTarget<'_>, &Claim, &Sha256Digest),
) -> Result<(Outcome, Vec<Call>), Box<dyn Error>> {
    published_bytes(case, (target, claim), (&case.bundle, digest))
}

/// Like [`published_as`], with `bundle` handed over in place of `case`'s.
fn published_bytes(
    case: &Case,
    (target, claim): (RedecideTarget<'_>, &Claim),
    (bundle, digest): (&[u8], &Sha256Digest),
) -> Result<(Outcome, Vec<Call>), Box<dyn Error>> {
    let rubric = rubric_v1()?;
    let world = case.claimed()?;
    world.calls.borrow_mut().clear();
    let outcome = publish_redecided(
        &RedecidedPersisted {
            target,
            claim,
            rubric: &rubric,
            bundle,
            digest,
            upload: PersistOutcome::Success,
        },
        &RedecidedPorts {
            forge: &world,
            ledger: &world,
            bundles: &case.stored(),
        },
    );
    let calls = world.calls.borrow().clone();
    Ok((outcome, calls))
}

#[test]
fn a_bundle_that_is_not_this_re_decision_is_refused_before_any_port() -> Result<(), Box<dyn Error>>
{
    let case = Case::plain()?;
    let other_evidence = Claim {
        evidence: assay_app::EvidencePointer {
            run_id: NonZeroU64::MAX,
            ..case.claim.evidence.clone()
        },
        ..case.claim.clone()
    };

    let digest = published_as(&case, (case.target(99)?, &case.claim, &digest('f')?))?;
    let run = published_as(&case, (case.target(100)?, &case.claim, &case.digest))?;
    let evidence = published_as(&case, (case.target(99)?, &other_evidence, &case.digest))?;

    assert_matches!(
        digest,
        (Err(RedecidedError::Bundle(RedecidedRefused::Digest)), calls) if calls.is_empty()
    );
    assert_matches!(
        run,
        (Err(RedecidedError::Bundle(RedecidedRefused::Run)), calls) if calls.is_empty()
    );
    assert_matches!(
        evidence,
        (Err(RedecidedError::Bundle(RedecidedRefused::Evidence)), calls) if calls.is_empty()
    );
    Ok(())
}

#[test]
fn a_bundle_of_another_attempt_head_or_base_is_refused_before_any_port()
-> Result<(), Box<dyn Error>> {
    let case = Case::plain()?;
    let rerun = RedecideTarget {
        run_uid: RunUid {
            run_attempt: NonZeroU64::new(2).ok_or("not zero")?,
            ..decide_only_run(99)?
        },
        ..case.target(99)?
    };
    let other_head = "c".repeat(40).parse()?;
    let at = |target| published_as(&case, (target, &case.claim, &case.digest));
    assert_matches!(
        refused(&at(rerun)?),
        Some(RedecidedRefused::NotFirstAttempt)
    );
    assert_matches!(
        refused(&at(RedecideTarget {
            head_sha: &other_head,
            ..case.target(99)?
        })?),
        Some(RedecidedRefused::Head)
    );
    assert_matches!(
        refused(&at(RedecideTarget {
            base_ref: "release",
            ..case.target(99)?
        })?),
        Some(RedecidedRefused::BaseRef)
    );
    Ok(())
}

#[test]
fn bytes_that_are_not_this_re_decision_are_refused_before_any_port() -> Result<(), Box<dyn Error>> {
    let case = Case::plain()?;
    let given = |bytes: &[u8]| {
        published_bytes(
            &case,
            (case.target(99)?, &case.claim),
            (bytes, &Sha256Digest::of(bytes)),
        )
    };
    let other_rubric = edited(&case, |bundle| {
        bundle["rubric_version"] = Value::from("0");
        bundle["decision"]["rubric_version"] = Value::from("0");
    })?;
    let other_decision = edited(&case, |bundle| {
        bundle["decision"]["conclusion"] = Value::from("failure");
    })?;
    let oversize = vec![b' '; assay_app::MAX_DECISION_BUNDLE_BYTES + 1];
    assert_matches!(
        refused(&given(&other_rubric)?),
        Some(RedecidedRefused::RubricVersion)
    );
    assert_matches!(
        refused(&given(&other_decision)?),
        Some(RedecidedRefused::Decision(_))
    );
    assert_matches!(
        refused(&given(&oversize)?),
        Some(RedecidedRefused::TooLarge)
    );
    assert_matches!(
        refused(&given(b"not json")?),
        Some(RedecidedRefused::Unreadable(_))
    );
    Ok(())
}

/// The refusal of a bundle refused before any port call; `None` for any other outcome.
fn refused((outcome, calls): &(Outcome, Vec<Call>)) -> Option<&RedecidedRefused> {
    match outcome {
        Err(RedecidedError::Bundle(refused)) if calls.is_empty() => Some(refused),
        _ => None,
    }
}

/// `case`'s bundle with `edit` applied to its JSON, re-encoded.
fn edited(case: &Case, edit: impl FnOnce(&mut Value)) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut bundle: Value = serde_json::from_slice(&case.bundle)?;
    edit(&mut bundle);
    Ok(serde_json::to_vec(&bundle)?)
}

#[test]
fn an_upload_stored_over_its_cap_is_unconfirmed_as_too_large() -> Result<(), Box<dyn Error>> {
    let case = Case::plain()?;
    let oversize = vec![b' '; assay_app::MAX_DECISION_BUNDLE_BYTES + 1];
    for stored in [Stored::TooLarge, Stored::Bytes(oversize)] {
        let world = case.claimed()?;

        let outcome = case.publish(&world, PersistOutcome::Success, &Uploaded::new(stored))?;

        let Ok(Published::Written(written)) = outcome else {
            return Err(format!("expected a write, got {outcome:?}").into());
        };
        assert_matches!(
            written.persist,
            Some(PersistFailure::Upload(Unconfirmed::TooLarge))
        );
        assert!(world.decisions.borrow().is_empty());
    }
    Ok(())
}

#[test]
fn a_re_decision_lists_the_approvals_its_snapshot_ignored() -> Result<(), Box<dyn Error>> {
    let case = Case::approved_before_push()?;
    let world = case.claimed()?;

    let outcome = case.publish(&world, PersistOutcome::Success, &case.stored())?;

    assert_matches!(outcome, Ok(Published::Written(_)));
    let summary = world.app_check(NAME)?.output.ok_or("written")?.summary;
    assert!(summary.contains("Ignored approvals:"), "{summary}");
    assert!(summary.contains("(reviewer 1002)"), "{summary}");
    Ok(())
}

/// What `publish_redecide_refusal` returns over the fakes.
type RefusalOutcome =
    Result<Published<FakeError, std::convert::Infallible>, PublishError<FakeError, FakeError>>;

/// Publishes `refusal` for decide-only run `run_id` over `claim`.
fn refuse(
    case: &Case,
    world: &FakeWorld,
    (run_id, refusal, claim): (u64, RedecideRefusal, &Claim),
) -> Result<RefusalOutcome, Box<dyn Error>> {
    Ok(refuse_at(world, (case.target(run_id)?, refusal, claim)))
}

/// Publishes `refusal` for `target` over `claim`.
fn refuse_at(
    world: &FakeWorld,
    (target, refusal, claim): (RedecideTarget<'_>, RedecideRefusal, &Claim),
) -> RefusalOutcome {
    publish_redecide_refusal(
        &UnusableEvidence {
            target,
            claim,
            refusal,
        },
        world,
        world,
    )
}

/// The key the refusal of `claim`'s evidence for `target` is published under, over a world
/// holding `claim`.
fn refusal_key(
    case: &Case,
    (target, refusal, claim): (RedecideTarget<'_>, RedecideRefusal, &Claim),
) -> Result<String, Box<dyn Error>> {
    let world = case.claimed()?;
    // The world's ledger holds its own copy of the claim's pointer.
    *world.pointer.borrow_mut() = Some(claim.evidence.clone());
    let outcome = refuse_at(&world, (target, refusal, claim));
    assert_matches!(outcome, Ok(Published::Written(_)));
    Ok(world.app_check(NAME)?.external_id)
}

#[test]
fn a_refusal_s_key_differs_by_reason_evidence_and_repository() -> Result<(), Box<dyn Error>> {
    let case = Case::plain()?;
    // A second claim of its own, the first one kept.
    let other_evidence = Claim {
        evidence: assay_app::EvidencePointer {
            run_id: NonZeroU64::MAX,
            ..case.claim.evidence.clone()
        },
        ..case.claim.clone()
    };
    let other_repository = RedecideTarget {
        run_uid: RunUid {
            repo_id: NonZeroU64::MAX,
            ..decide_only_run(99)?
        },
        ..case.target(99)?
    };
    let forged = (case.target(99)?, RedecideRefusal::Forged, &case.claim);

    let base = refusal_key(&case, forged)?;
    let keys = [
        refusal_key(
            &case,
            (
                case.target(99)?,
                RedecideRefusal::Absent(Absence::Expired),
                &case.claim,
            ),
        )?,
        refusal_key(
            &case,
            (
                case.target(99)?,
                RedecideRefusal::Absent(Absence::Missing),
                &case.claim,
            ),
        )?,
        refusal_key(
            &case,
            (case.target(99)?, RedecideRefusal::Forged, &other_evidence),
        )?,
        refusal_key(
            &case,
            (other_repository, RedecideRefusal::Forged, &case.claim),
        )?,
    ];

    assert_eq!(
        refusal_key(&case, forged)?,
        base,
        "the key is deterministic"
    );
    for (index, key) in keys.iter().enumerate() {
        assert_ne!(*key, base, "{index}");
        assert!(key.starts_with("sha256:"), "{key}");
    }
    let [expired, missing, ..] = &keys;
    assert_ne!(expired, missing, "the detail is keyed");
    Ok(())
}

#[test]
fn a_refusal_for_a_re_run_is_refused_before_any_port() -> Result<(), Box<dyn Error>> {
    let case = Case::plain()?;
    let world = case.claimed()?;
    world.calls.borrow_mut().clear();
    let rerun = RedecideTarget {
        run_uid: RunUid {
            run_attempt: NonZeroU64::new(2).ok_or("not zero")?,
            ..decide_only_run(99)?
        },
        ..case.target(99)?
    };

    let outcome = refuse_at(&world, (rerun, RedecideRefusal::Forged, &case.claim));

    assert_matches!(outcome, Err(PublishError::NotFirstAttempt));
    assert_eq!(*world.calls.borrow(), []);
    Ok(())
}

#[test]
fn unusable_evidence_is_refused_over_the_register_s_key() -> Result<(), Box<dyn Error>> {
    let case = Case::plain()?;
    let refusals = [
        (
            RedecideRefusal::Forged,
            "gate-error",
            "gate-error: bundle-digest",
        ),
        (
            RedecideRefusal::Absent(Absence::Expired),
            "bundle-expired",
            "bundle-expired: expired",
        ),
        (
            RedecideRefusal::Absent(Absence::Missing),
            "bundle-expired",
            "bundle-expired: missing",
        ),
    ];
    for (refusal, code, reason) in refusals {
        let world = case.claimed()?;

        let outcome = refuse(&case, &world, (99, refusal, &case.claim))?;

        let Ok(Published::Written(written)) = outcome else {
            return Err(format!("{code}: expected a write, got {outcome:?}").into());
        };
        assert_eq!(
            written.state,
            CheckState::Completed(CheckConclusion::Failure)
        );
        let check = world.app_check(NAME)?;
        let output = check.output.ok_or("written")?;
        assert_eq!(output.title, format!("assay: failure ({code})"));
        assert!(output.summary.contains(reason), "{}", output.summary);
        assert!(
            output.text.contains("evidence pointer: none"),
            "{}",
            output.text
        );
        assert_eq!(
            world.writes(),
            [
                Call::Acquire,
                Call::RecordPublished,
                Call::UpdateCheck,
                Call::CreateSummary,
                Call::Release
            ],
            "{code}: the register alone, before the check"
        );
        let published = world.published.borrow();
        let [entry] = published.as_slice() else {
            return Err(format!("one register entry, got {}", published.len()).into());
        };
        assert_eq!(entry.register.key.as_str(), check.external_id);
        assert_eq!(entry.register.run_id, decide_only_run(99)?.run_id);
        assert_eq!(entry.over.as_ref(), Some(&case.claim));
    }
    Ok(())
}

#[test]
fn the_same_refusal_twice_is_written_once() -> Result<(), Box<dyn Error>> {
    let case = Case::plain()?;
    let world = case.claimed()?;
    refuse(&case, &world, (99, RedecideRefusal::Forged, &case.claim))??;
    let reread = Claim {
        register: world.register.borrow().clone().ok_or("a register")?,
        ..case.claim.clone()
    };
    world.calls.borrow_mut().clear();

    let again = refuse(&case, &world, (100, RedecideRefusal::Forged, &reread))?;

    assert_matches!(again, Ok(Published::Unchanged));
    assert_eq!(world.writes(), [], "the poll's repeat writes nothing");
    Ok(())
}

#[test]
fn a_refusal_whose_claim_moved_under_the_lock_yields() -> Result<(), Box<dyn Error>> {
    let case = Case::plain()?;
    let world = case.claimed()?;
    if let Some(register) = world.register.borrow_mut().as_mut() {
        register.run_id = NonZeroU64::MAX;
    }

    let outcome = refuse(&case, &world, (99, RedecideRefusal::Forged, &case.claim))?;

    assert_matches!(
        outcome,
        Ok(Published::Yielded {
            stale: Stale::Moved,
            unlock: Unlock::Released
        })
    );
    assert_eq!(world.app_check(NAME)?.output, None);
    Ok(())
}
