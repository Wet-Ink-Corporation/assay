//! `run_publish` (#60 ST-14, Z4, D5 A) over the state `run_decide` wrote for a fixture PR and
//! fakes of the verdict check, the per-head lock and the artifact store: a decided run whose
//! upload the store confirms by sha256 records its evidence, an upload that did not succeed
//! fails closed with `persist-error`, a refusal publishes the refusal check, and a silent run
//! publishes nothing.

use std::cell::RefCell;
use std::error::Error;
use std::fs;

use assay_app::{
    BUNDLE_FILE, CheckConclusion, CheckState, PersistFailure, PersistOutcome, PersistedError,
    Published, StoredBundle, Unconfirmed,
};
use assay_cli::{PublishJob, PublishJobPorts, PublishRun, PublishRunError, run_publish};
use assay_domain::Aeu;

use crate::decide_fakes::{FreeLock, Uploaded, VerdictCheck};
use crate::decide_run::{Planned, moved};
use crate::open_fakes::Refused;

/// What `run_publish` gives over the fakes.
type Ran = Result<PublishRun<Refused, Refused>, PublishRunError<Refused, Refused>>;

/// The fakes `publish` writes through, the check stamped by `open` for the fixture's run.
struct World {
    check: VerdictCheck,
    lock: FreeLock,
    store: Uploaded,
}

impl World {
    /// The check as `open` left it, and `stored` as the upload step left the artifact.
    fn new(stored: StoredBundle) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            check: VerdictCheck::stamped(4242)?,
            lock: FreeLock::default(),
            store: Uploaded {
                stored,
                asked: RefCell::default(),
            },
        })
    }

    /// Publishes `planned`'s decide state after an upload that ended `upload`.
    fn publish(&self, planned: &Planned, upload: PersistOutcome) -> Ran {
        let job = PublishJob {
            run: &planned.run,
            identity: &planned.needs.identity,
            cost: Aeu::from_micros(0),
            rubric: &planned.rubric,
            upload,
        };
        let ports = PublishJobPorts {
            forge: &self.check,
            ledger: &self.lock,
            bundles: &self.store,
        };
        run_publish(&job, &ports, &planned.out())
    }
}

/// A fixture PR decided as `decide` decides it, its bundle in decide's `--out`.
fn decided() -> Result<(Planned, Vec<u8>), Box<dyn Error>> {
    let planned = Planned::new()?;
    planned.decide(&planned.unmoved())??;
    let bundle = fs::read(planned.out().join(BUNDLE_FILE))?;
    Ok((planned, bundle))
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn an_upload_the_store_confirms_records_the_evidence() -> Result<(), Box<dyn Error>> {
    let (planned, bundle) = decided()?;
    let world = World::new(StoredBundle::Found(bundle))?;

    let ran = world.publish(&planned, PersistOutcome::Success)?;

    let PublishRun::Decided(Published::Written(written)) = ran else {
        return Err(format!("not written: {ran:?}").into());
    };
    assert!(written.persist.is_none(), "{:?}", written.persist);
    assert_eq!(
        world.store.asked.borrow().len(),
        1,
        "the store is read once"
    );
    assert_eq!(
        world.lock.evidence.borrow().len(),
        1,
        "the evidence is recorded"
    );
    let update = world.check.written().ok_or("no check written")?;
    assert_eq!(update.state, written.state);
    assert!(
        !update.summary.contains("persist-error"),
        "{}",
        update.summary
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn an_upload_the_store_does_not_confirm_records_no_evidence() -> Result<(), Box<dyn Error>> {
    let (planned, mut bundle) = decided()?;
    bundle.push(b'\n');
    let world = World::new(StoredBundle::Found(bundle))?;

    let ran = world.publish(&planned, PersistOutcome::Success)?;

    let PublishRun::Decided(Published::Written(written)) = ran else {
        return Err(format!("not written: {ran:?}").into());
    };
    assert!(
        matches!(
            written.persist,
            Some(PersistFailure::Upload(Unconfirmed::Digest))
        ),
        "{:?}",
        written.persist
    );
    assert!(world.lock.evidence.borrow().is_empty(), "no evidence");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn an_upload_that_did_not_succeed_fails_an_exec_surface_pr_with_persist_error()
-> Result<(), Box<dyn Error>> {
    let (planned, bundle) = decided()?;
    for upload in [PersistOutcome::Failure, PersistOutcome::Skipped] {
        // Each case's store holds its own copy of what was uploaded.
        let world = World::new(StoredBundle::Found(bundle.clone()))?;

        let ran = world.publish(&planned, upload)?;

        let PublishRun::Decided(Published::Written(written)) = ran else {
            return Err(format!("{upload}: not written: {ran:?}").into());
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
            "{upload}"
        );
        let update = world.check.written().ok_or("no check written")?;
        assert!(
            update.summary.contains("persist-error"),
            "{upload}: {}",
            update.summary
        );
        assert!(world.store.asked.borrow().is_empty(), "{upload}: unread");
        assert!(
            world.lock.evidence.borrow().is_empty(),
            "{upload}: no evidence"
        );
    }
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_bundle_that_is_not_the_states_is_refused_before_any_port() -> Result<(), Box<dyn Error>> {
    let (planned, mut bundle) = decided()?;
    bundle.push(b'\n');
    fs::write(planned.out().join(BUNDLE_FILE), &bundle)?;
    let world = World::new(StoredBundle::Found(bundle))?;

    let ran = world.publish(&planned, PersistOutcome::Success);

    assert!(
        matches!(
            ran,
            Err(PublishRunError::Persisted(PersistedError::Bundle(_)))
        ),
        "{ran:?}"
    );
    assert!(world.check.calls.borrow().is_empty(), "no check read");
    assert!(world.lock.calls.borrow().is_empty(), "no lock taken");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_refusal_publishes_the_refusal_check_and_persists_nothing() -> Result<(), Box<dyn Error>> {
    let planned = Planned::new()?;
    planned.tamper_gates_static()?;
    planned.decide(&planned.unmoved())??;
    let world = World::new(StoredBundle::Missing)?;

    let ran = world.publish(&planned, PersistOutcome::Skipped)?;

    let PublishRun::Refused(Published::Written(written)) = ran else {
        return Err(format!("not written: {ran:?}").into());
    };
    assert_eq!(
        written.state,
        CheckState::Completed(CheckConclusion::Failure)
    );
    let update = world.check.written().ok_or("no check written")?;
    assert_eq!(update.title, "assay: failure (gate-error)");
    assert!(world.store.asked.borrow().is_empty(), "no store read");
    assert!(world.lock.evidence.borrow().is_empty(), "no evidence");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_silent_run_publishes_nothing() -> Result<(), Box<dyn Error>> {
    let planned = Planned::new()?;
    planned.decide(&moved()?)??;
    let world = World::new(StoredBundle::Missing)?;

    let ran = world.publish(&planned, PersistOutcome::Skipped)?;

    assert!(matches!(ran, PublishRun::Silent), "{ran:?}");
    assert!(world.check.calls.borrow().is_empty(), "no check read");
    assert!(world.lock.calls.borrow().is_empty(), "no lock taken");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_missing_state_is_refused_before_any_port() -> Result<(), Box<dyn Error>> {
    let planned = Planned::new()?;
    let world = World::new(StoredBundle::Missing)?;

    let ran = world.publish(&planned, PersistOutcome::Skipped);

    assert!(
        matches!(ran, Err(PublishRunError::Missing { .. })),
        "{ran:?}"
    );
    assert!(world.check.calls.borrow().is_empty(), "no check read");
    Ok(())
}
