//! The pipeline's verdict writers record the publication register (#61 DO-7, D2 A): `publish`
//! and `publish_persisted` with the evidence pointer, a refusal alone, under the per-head lock
//! and before the check; a decision that persisted nothing leaves the register as it was.

use std::error::Error;

use assay_app::{
    Decided, PersistOutcome, Persisted, PersistedPorts, PublicationRegister, Published,
    PublishedEntry, RefusalReason, Sha256Digest, prepare, publish_persisted, publish_refusal,
};
use assay_domain::{Reason, ReasonCode};

use crate::publish_fakes::{Call, FakeWorld};
use crate::publish_support::{KEY, NAME, Run, stamped, written};
use crate::replay_fakes::{FakeBundles, Stored};

/// Run `run`'s register of its head under `key`, as a pipeline writer records it.
fn pipeline_register(run: &Run, key: &str) -> Result<PublishedEntry, Box<dyn Error>> {
    let target = run.target();
    Ok(PublishedEntry {
        repo_id: target.run_uid.repo_id,
        pr: target.pull_request.number,
        register: PublicationRegister {
            // The entry owns its copy of the head.
            head_sha: target.pull_request.head_sha.clone(),
            key: key.parse::<Sha256Digest>()?,
            run_id: target.run_uid.run_id,
        },
        over: None,
    })
}

/// The position of the first `call` in `world`'s log.
fn position(world: &FakeWorld, call: Call) -> Result<usize, Box<dyn Error>> {
    world
        .calls
        .borrow()
        .iter()
        .position(|made| *made == call)
        .ok_or_else(|| format!("no {call:?} in {:?}", world.calls.borrow()).into())
}

#[test]
fn publish_records_the_register_after_the_evidence_and_before_the_check()
-> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let world = stamped()?;

    written(run.publish(&world))?;

    assert_eq!(*world.published.borrow(), [pipeline_register(&run, KEY)?]);
    let (evidence, register, check) = (
        position(&world, Call::RecordEvidence)?,
        position(&world, Call::RecordPublished)?,
        position(&world, Call::UpdateCheck)?,
    );
    assert!(evidence < register && register < check, "{:?}", world.calls);
    assert!(
        position(&world, Call::Acquire)? < evidence,
        "under the lock"
    );
    Ok(())
}

#[test]
fn a_decision_that_persisted_nothing_leaves_the_register() -> Result<(), Box<dyn Error>> {
    for failing in [Call::Upload, Call::RecordEvidence] {
        let run = Run::new()?;
        let world = stamped()?;
        world.fail(failing, 1);

        written(run.publish(&world))?;

        assert!(world.published.borrow().is_empty(), "{failing:?}");
        assert!(
            !world.writes().contains(&Call::RecordPublished),
            "{failing:?}"
        );
    }
    Ok(())
}

#[test]
fn a_refusal_records_its_register_alone_before_the_check() -> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let world = stamped()?;
    let refusal = RefusalReason::try_from(Reason {
        code: ReasonCode::GateError,
        detail: "plan: failure".to_owned(),
        fp: None,
    })?;

    let outcome = publish_refusal(&run.target(), &refusal, &world, &world);

    assert!(matches!(outcome, Ok(Published::Written(_))), "{outcome:?}");
    let key = world.app_check(NAME)?.external_id;
    assert_eq!(*world.published.borrow(), [pipeline_register(&run, &key)?]);
    assert!(world.evidence.borrow().is_empty(), "no evidence");
    assert!(position(&world, Call::RecordPublished)? < position(&world, Call::UpdateCheck)?);
    Ok(())
}

#[test]
fn publish_persisted_records_the_register_with_the_evidence() -> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let world = stamped()?;
    let bundle = prepare(
        &run.target(),
        &Decided {
            run: &run.authenticated,
            rubric: &run.rubric,
            inputs: &run.inputs,
            events: &run.events,
            carry: &run.carry,
            github: &run.github,
            cost: run.cost,
        },
    )?;
    let bundles = FakeBundles::new(Stored::Bytes(bundle.bytes().to_vec()));

    let outcome = publish_persisted(
        &Persisted {
            target: run.target(),
            rubric: &run.rubric,
            cost: run.cost,
            bundle: bundle.bytes(),
            digest: bundle.digest(),
            upload: PersistOutcome::Success,
        },
        &PersistedPorts {
            forge: &world,
            ledger: &world,
            bundles: &bundles,
        },
    );

    assert!(matches!(outcome, Ok(Published::Written(_))), "{outcome:?}");
    assert_eq!(world.evidence.borrow().len(), 1);
    assert_eq!(*world.published.borrow(), [pipeline_register(&run, KEY)?]);
    assert!(position(&world, Call::RecordEvidence)? < position(&world, Call::RecordPublished)?);
    assert!(position(&world, Call::RecordPublished)? < position(&world, Call::UpdateCheck)?);
    Ok(())
}
