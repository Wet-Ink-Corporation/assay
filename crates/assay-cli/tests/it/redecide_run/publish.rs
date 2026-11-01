//! `run_republish` (#61 DO-12) over what `run_redecide` wrote for the fixture PR: the App's
//! verdict check (a fake, written under the register's key), the run's artifact store (a
//! fake) and the per-head lock through a real `LedgerPublish` over the scene's ledger, so the
//! decision pointer and the register are read back from the ledger state.

use std::error::Error;
use std::fs;
use std::path::Path;

use assay_app::{
    Absence, CheckConclusion, CheckState, DECISION_FILE, DecisionPointer, LedgerPublish,
    PersistFailure, PersistOutcome, PublicationRegister, Published, RedecideRefusal, Sha256Digest,
    StoredBundle, Unconfirmed,
};
use assay_cli::outputs::{DecideOnlyGuardOutputs, DecideOnlyPrs, DecideTrigger};
use assay_cli::{
    RedecideState, RepublishJob, RepublishJobPorts, RepublishRun, RepublishRunError, STATE_FILE,
    run_republish,
};

use super::{
    DECIDE_ONLY_RUN, LATER, NOW, OWNER, PIPELINE_RUN, PR, Redecided, Scene, comment, id, label,
    override_line,
};
use crate::open_fakes::{FixedClock, MemoryRefs, Refused};
use crate::redecide_fakes::LoggedLedger;
use crate::republish_fakes::{Store, Verdict, completed_under};

/// What `run_republish` gives over the fakes.
type Ran = Result<
    RepublishRun<assay_app::LedgerPublishError<Refused, Refused, Refused>, Refused>,
    RepublishRunError<Refused, Refused, assay_app::LedgerPublishError<Refused, Refused, Refused>>,
>;

/// The key the scene's register names: the pipeline's, which no re-decision gives.
fn register_key() -> Sha256Digest {
    Sha256Digest::of(b"an older decision")
}

/// GitHub and the lock as `decide-only publish` finds them after `scene`'s decide step.
struct World<'s> {
    scene: &'s Scene,
    verdict: Verdict<'s>,
    store: Store<'s>,
    refs: MemoryRefs,
    clock: FixedClock,
}

impl<'s> World<'s> {
    /// The check completed under the register's key, the PR unmoved, and `stored` as the
    /// upload step left the decision artifact.
    fn new(scene: &'s Scene, stored: StoredBundle) -> Result<Self, Box<dyn Error>> {
        Self::checked(scene, stored, &register_key())
    }

    /// Like [`World::new`], with the check completed under `key`.
    fn checked(
        scene: &'s Scene,
        stored: StoredBundle,
        key: &Sha256Digest,
    ) -> Result<Self, Box<dyn Error>> {
        // Only what the publish step calls is logged from here on.
        scene.log.borrow_mut().clear();
        Ok(Self {
            scene,
            verdict: Verdict {
                current: scene.hub()?.current,
                check: completed_under(key)?,
                log: &scene.log,
            },
            store: Store {
                stored,
                log: &scene.log,
            },
            refs: MemoryRefs::default(),
            clock: FixedClock::at(NOW.parse()?),
        })
    }

    /// Publishes `out` for PR `pr` (one of guard's) after an upload that ended `upload`.
    fn publish(
        &self,
        out: &Path,
        scheduled: u64,
        upload: PersistOutcome,
    ) -> Result<Ran, Box<dyn Error>> {
        let scene = self.scene;
        let ledger = LoggedLedger {
            files: &scene.files,
            log: &scene.log,
            refuse: None,
        };
        let lock = LedgerPublish {
            ledger: &ledger,
            refs: &self.refs,
            clock: &self.clock,
            run_uid: scene.run.run_env().run_uid,
            repo: "Wet-Ink-Corporation/assay",
        };
        let guard =
            DecideOnlyGuardOutputs::new(DecideOnlyPrs::one(id(scheduled)?), DecideTrigger::Comment);
        Ok(run_republish(
            &RepublishJob {
                run: &scene.run,
                guard: &guard,
                pr: id(PR)?,
                rubric: &scene.planned.rubric,
                upload,
            },
            &RepublishJobPorts {
                app: &self.verdict,
                run: &self.store,
                ledger: &ledger,
                lock: &lock,
            },
            out,
        ))
    }
}

/// The owner's override decided by decide-only, its bundle in `--out`.
fn overridden(scene: &Scene) -> Result<(Redecided, Vec<u8>), Box<dyn Error>> {
    let hub = scene.hub()?;
    let ran = scene.redecide(
        &hub,
        &comment(OWNER, &override_line(scene), LATER)?,
        DecideTrigger::Comment,
    )?;
    let bundle = fs::read(ran.out.join(DECISION_FILE))?;
    Ok((ran, bundle))
}

/// The decision key the bundle records.
fn decision_key(bundle: &[u8]) -> Result<Sha256Digest, Box<dyn Error>> {
    let value: serde_json::Value = serde_json::from_slice(bundle)?;
    let key = value["inputs"]["decision_key"]
        .as_str()
        .ok_or("a decision key")?;
    Ok(key.parse()?)
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn an_upload_the_store_confirms_records_the_decision_pointer_and_register()
-> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let (ran, bundle) = overridden(&scene)?;
    // The store holds its own copy of what was uploaded; the test keeps one to compare.
    let world = World::new(&scene, StoredBundle::Found(bundle.clone()))?;

    let published = world.publish(&ran.out, PR, PersistOutcome::Success)??;

    let RepublishRun::Decided(Published::Written(written)) = published else {
        return Err(format!("not written: {published:?}").into());
    };
    assert!(written.persist.is_none(), "{:?}", written.persist);
    let key = decision_key(&bundle)?;
    let state = scene.state()?;
    assert_eq!(
        state.decision,
        Some(DecisionPointer {
            run_id: id(DECIDE_ONLY_RUN.parse()?)?,
            sha256: Sha256Digest::of(&bundle),
        })
    );
    assert_eq!(
        state.published,
        Some(PublicationRegister {
            head_sha: scene.head(),
            // The expected register owns its key; the check's id is compared after.
            key: key.clone(),
            run_id: id(DECIDE_ONLY_RUN.parse()?)?,
        })
    );
    let update = world.verdict.check.written().ok_or("no check written")?;
    assert_eq!(update.external_id, key.to_string());
    let fetched = format!("fetch decision {DECIDE_ONLY_RUN} {PR}");
    let log = scene.log.borrow();
    assert_eq!(
        log.iter().filter(|call| **call == fetched).count(),
        1,
        "the store is read once: {log:?}"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn an_upload_that_did_not_succeed_writes_no_pointer_and_fails_with_persist_error()
-> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let (ran, bundle) = overridden(&scene)?;
    let before = scene.state()?;
    for upload in [PersistOutcome::Failure, PersistOutcome::Skipped] {
        // Each case's store holds its own copy of what was uploaded.
        let world = World::new(&scene, StoredBundle::Found(bundle.clone()))?;

        let published = world.publish(&ran.out, PR, upload)??;

        let RepublishRun::Decided(Published::Written(written)) = published else {
            return Err(format!("{upload}: not written: {published:?}").into());
        };
        assert!(
            matches!(
                written.persist,
                Some(PersistFailure::Upload(Unconfirmed::NotUploaded(outcome))) if outcome == upload
            ),
            "{upload}: {:?}",
            written.persist
        );
        assert_eq!(
            written.state,
            CheckState::Completed(CheckConclusion::Failure),
            "{upload}: the fixture PR is exec-surface"
        );
        let update = world.verdict.check.written().ok_or("no check written")?;
        assert!(
            update.summary.contains("persist-error"),
            "{upload}: {}",
            update.summary
        );
        assert!(
            !scene
                .log
                .borrow()
                .iter()
                .any(|call| call.starts_with("fetch decision")),
            "{upload}: the store is not read"
        );
        let after = scene.state()?;
        assert_eq!(
            (after.decision, after.published),
            // The expected pair owns its register; `before` serves every case.
            (None, before.published.clone()),
            "{upload}: no decision pointer, and the register is the pipeline's"
        );
    }
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_refusal_is_published_over_the_register_and_once() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let mut hub = scene.hub()?;
    hub.stored = StoredBundle::Expired;
    let ran = scene.redecide(&hub, &label(&scene)?, DecideTrigger::Label)?;
    assert_eq!(
        ran.state()?,
        RedecideState::Refused(RedecideRefusal::Absent(Absence::Expired))
    );
    let world = World::new(&scene, StoredBundle::Missing)?;

    let published = world.publish(&ran.out, PR, PersistOutcome::Skipped)??;

    let RepublishRun::Refused(Published::Written(written)) = published else {
        return Err(format!("not written: {published:?}").into());
    };
    assert_eq!(
        written.state,
        CheckState::Completed(CheckConclusion::Failure)
    );
    let update = world.verdict.check.written().ok_or("no check written")?;
    assert_eq!(update.title, "assay: failure (bundle-expired)");
    let state = scene.state()?;
    let evidence = state.evidence.as_ref().ok_or("an evidence pointer")?;
    let register = state.published.as_ref().ok_or("a register")?;
    assert_eq!(update.external_id, register.key.to_string());
    assert!(
        RedecideRefusal::keyed_over(id(super::REPO)?, evidence, &register.key),
        "the register names the refusal's key"
    );
    assert_eq!(
        (register.run_id, &register.head_sha),
        (id(DECIDE_ONLY_RUN.parse()?)?, &scene.head())
    );
    assert_eq!(state.decision, None, "a refusal writes no decision pointer");

    let again = World::checked(&scene, StoredBundle::Missing, &register.key)?;
    let repeated = again.publish(&ran.out, PR, PersistOutcome::Skipped)??;

    assert!(
        matches!(repeated, RepublishRun::Refused(Published::Unchanged)),
        "{repeated:?}"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_silent_state_calls_no_port() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let out = scene.planned.root.join("silent-out");
    fs::create_dir(&out)?;
    fs::write(out.join(STATE_FILE), RedecideState::Silent.json())?;
    let world = World::new(&scene, StoredBundle::Missing)?;

    let published = world.publish(&out, PR, PersistOutcome::Skipped)??;

    assert!(matches!(published, RepublishRun::Silent), "{published:?}");
    assert_no_port(&world);
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_bundle_that_is_not_the_states_is_refused_before_any_port() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let (ran, mut bundle) = overridden(&scene)?;
    bundle.push(b'\n');
    fs::write(ran.out.join(DECISION_FILE), &bundle)?;
    let world = World::new(&scene, StoredBundle::Found(bundle))?;

    let published = world.publish(&ran.out, PR, PersistOutcome::Success)?;

    assert!(
        matches!(published, Err(RepublishRunError::Digest)),
        "{published:?}"
    );
    assert_no_port(&world);
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_pr_guard_did_not_schedule_or_a_missing_state_is_refused_before_any_port()
-> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let (ran, bundle) = overridden(&scene)?;
    let world = World::new(&scene, StoredBundle::Found(bundle))?;

    let unscheduled = world.publish(&ran.out, 56, PersistOutcome::Success)?;
    let missing = world.publish(
        &scene.planned.root.join("no-out"),
        PR,
        PersistOutcome::Success,
    )?;

    assert!(
        matches!(unscheduled, Err(RepublishRunError::NotScheduled { pr }) if pr.get() == PR),
        "{unscheduled:?}"
    );
    assert!(
        matches!(missing, Err(RepublishRunError::Missing { .. })),
        "{missing:?}"
    );
    assert_no_port(&world);
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_register_another_run_wrote_since_decide_is_not_published_over() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let (ran, bundle) = overridden(&scene)?;
    let newer = Sha256Digest::of(b"a newer run's refusal");
    scene.register(&scene.head(), &newer, id(PIPELINE_RUN.saturating_add(1))?)?;
    let before = scene.state()?;
    let world = World::checked(&scene, StoredBundle::Found(bundle), &newer)?;

    let published = world.publish(&ran.out, PR, PersistOutcome::Success)??;

    assert!(
        matches!(published, RepublishRun::Unclaimed),
        "{published:?}"
    );
    assert!(world.verdict.check.written().is_none(), "no check written");
    assert!(world.refs.created().is_empty(), "no lock taken");
    assert_eq!(scene.state()?, before, "the ledger state is left");
    Ok(())
}

/// Asserts that nothing was read from GitHub or the ledger, and no lock taken.
fn assert_no_port(world: &World<'_>) {
    let log = world.scene.log.borrow();
    assert!(log.is_empty(), "no port is called: {log:?}");
    assert!(world.refs.created().is_empty(), "no lock taken");
}
