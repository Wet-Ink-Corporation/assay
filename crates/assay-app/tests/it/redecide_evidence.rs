//! decide-only's evidence read (J4, F19, F20, D11): the bundle the evidence pointer names is
//! believed only from attempt 1 of the pipeline workflow of this repository, read after its
//! run's provenance and authenticated as replay authenticates it. Forgery, absence and another
//! rubric are values; only a port failure is an error.

use std::assert_matches;
use std::cell::RefCell;
use std::error::Error;
use std::num::NonZeroU64;

use assay_app::Absence;
use assay_app::{
    BUNDLE_FILE, BundleShape, BundleSource, Carry, EvidenceError, EvidencePointer, EvidenceRead,
    EvidenceRequest, Forgery, MAX_BUNDLE_BYTES, Mismatch, PriorCarry, ProvenanceRefusal, RunEvent,
    RunProvenance, Sha256Digest, StoredBundle, WorkflowRunFacts, prior_waivers, read_evidence,
};
use assay_domain::{Decision, Rubric, RunUid};
use serde_json::Value;

use crate::plan_fakes::{FakeError, HEAD};
use crate::replay_fakes::{FakeBundles, Persisted, Stored};
use crate::support::rubric_v1;
use crate::waive_carry::{by_approver, prior_bundle};

/// A port call, in the order the use case made it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Call {
    /// The run's provenance was read.
    Provenance(NonZeroU64),
    /// An artifact was fetched.
    Fetch,
}

/// What the fake GitHub answers for the run.
#[derive(Debug, Clone)]
enum Answer {
    /// These facts.
    Run(WorkflowRunFacts),
    /// No such run.
    NoRun,
    /// GitHub cannot be read.
    Fails,
}

/// The run reader and the artifact store, logging every call into one list.
struct Ports {
    answer: Answer,
    bundles: FakeBundles,
    calls: RefCell<Vec<Call>>,
}

impl RunProvenance for Ports {
    type Error = FakeError;

    fn workflow_run(&self, run_id: NonZeroU64) -> Result<Option<WorkflowRunFacts>, FakeError> {
        self.calls.borrow_mut().push(Call::Provenance(run_id));
        match &self.answer {
            Answer::Run(facts) => Ok(Some(facts.clone())),
            Answer::NoRun => Ok(None),
            Answer::Fails => Err(FakeError),
        }
    }
}

impl BundleSource for Ports {
    type Error = FakeError;

    fn fetch(&self, run: RunUid, file: &str, limit: usize) -> Result<StoredBundle, FakeError> {
        self.calls.borrow_mut().push(Call::Fetch);
        self.bundles.fetch(run, file, limit)
    }
}

/// What `read_evidence` returns over the fakes.
type Outcome = Result<EvidenceRead, EvidenceError<FakeError, FakeError>>;

/// The pipeline's `pull_request_target` run that persisted `persisted`.
fn pipeline_run(persisted: &Persisted) -> WorkflowRunFacts {
    WorkflowRunFacts {
        run_id: persisted.entry.pointer.run_id,
        repo_id: persisted.entry.repo_id,
        path: ".github/workflows/assay.yml".to_owned(),
        event: RunEvent::PullRequestTarget,
        head_branch: "feature".to_owned(),
        head_repo: Some(persisted.entry.repo_id),
        run_attempt: NonZeroU64::MIN,
    }
}

impl Ports {
    /// GitHub answering `answer` and the store holding `stored`, with no call made yet.
    fn new(answer: Answer, stored: Stored) -> Self {
        Self {
            answer,
            bundles: FakeBundles::new(stored),
            calls: RefCell::new(Vec::new()),
        }
    }
}

/// Reads the evidence `pointer` names under `rubric`, over `ports`.
fn read_under(
    persisted: &Persisted,
    pointer: &EvidencePointer,
    ports: Ports,
    rubric: &Rubric,
) -> (Outcome, Vec<Call>) {
    let request = EvidenceRequest {
        repo_id: persisted.entry.repo_id,
        pointer,
        default_branch: "main",
        rubric,
    };
    let outcome = read_evidence(&request, &ports, &ports);
    (outcome, ports.calls.into_inner())
}

/// Reads the persisted pointer's evidence under rubric v1, over `answer` and `stored`.
fn read(
    persisted: &Persisted,
    answer: Answer,
    stored: Stored,
) -> Result<(Outcome, Vec<Call>), Box<dyn Error>> {
    Ok(read_under(
        persisted,
        &persisted.entry.pointer,
        Ports::new(answer, stored),
        &rubric_v1()?,
    ))
}

/// The persisted bundle, as stored.
fn stored(persisted: &Persisted) -> Stored {
    Stored::Bytes(persisted.bytes.clone())
}

/// The pipeline run with `edit` applied.
fn run_with(persisted: &Persisted, edit: impl FnOnce(&mut WorkflowRunFacts)) -> Answer {
    let mut facts = pipeline_run(persisted);
    edit(&mut facts);
    Answer::Run(facts)
}

/// The persisted bundle with `tamper` applied to its JSON, and a pointer re-aimed at the
/// result, as a writer of the ledger could re-point it.
fn tampered(
    persisted: &Persisted,
    tamper: impl FnOnce(&mut Value) -> Result<(), Box<dyn Error>>,
) -> Result<(Vec<u8>, EvidencePointer), Box<dyn Error>> {
    let mut bundle: Value = serde_json::from_slice(&persisted.bytes)?;
    tamper(&mut bundle)?;
    let bytes = serde_json::to_vec(&bundle)?;
    let pointer = EvidencePointer {
        sha256: Sha256Digest::of(&bytes),
        ..persisted.entry.pointer.clone()
    };
    Ok((bytes, pointer))
}

/// The provenance read, then one fetch.
fn read_then_fetched(persisted: &Persisted) -> Vec<Call> {
    vec![
        Call::Provenance(persisted.entry.pointer.run_id),
        Call::Fetch,
    ]
}

#[test]
fn a_bundle_of_the_pipeline_s_run_is_authentic_after_its_provenance_is_read()
-> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;
    let answer = Answer::Run(pipeline_run(&persisted));

    let (outcome, calls) = read(&persisted, answer, stored(&persisted))?;

    let Ok(EvidenceRead::Authentic(bundle)) = outcome else {
        return Err(format!("authentic, got {outcome:?}").into());
    };
    assert_eq!(calls, read_then_fetched(&persisted));
    let recorded: Value = serde_json::from_slice(&persisted.bytes)?;
    let decision: Decision = serde_json::from_value(recorded["decision"].clone())?;
    assert_eq!(bundle.decision(), &decision);
    assert_eq!(bundle.pointer(), &persisted.entry.pointer);
    assert_eq!(bundle.head().as_str(), HEAD);
    assert_eq!(bundle.base_ref(), "main");
    assert_eq!(bundle.inputs().decision_key, decision.key);
    Ok(())
}

#[test]
fn an_authentic_bundle_carries_what_its_decision_waived_with_its_binding()
-> Result<(), Box<dyn Error>> {
    let (run, bytes, pointer) = prior_bundle(by_approver)?;
    let repo_id = run.request.plan.run_uid.repo_id;
    let prior = PriorCarry {
        repo_id,
        pointer: &pointer,
        repository: "Wet-Ink-Corporation/assay",
    };
    let expected = prior_waivers(
        Carry::Kept,
        &prior,
        &FakeBundles::new(Stored::Bytes(bytes.clone())),
    )?;
    let ports = Ports {
        answer: Answer::Run(WorkflowRunFacts {
            run_id: pointer.run_id,
            repo_id,
            path: ".github/workflows/assay.yml".to_owned(),
            event: RunEvent::PullRequestTarget,
            head_branch: "feature".to_owned(),
            head_repo: Some(repo_id),
            run_attempt: NonZeroU64::MIN,
        }),
        bundles: FakeBundles::new(Stored::Bytes(bytes)),
        calls: RefCell::new(Vec::new()),
    };
    let rubric = rubric_v1()?;
    let request = EvidenceRequest {
        repo_id,
        pointer: &pointer,
        default_branch: "main",
        rubric: &rubric,
    };

    let outcome = read_evidence(&request, &ports, &ports);

    let Ok(EvidenceRead::Authentic(bundle)) = outcome else {
        return Err(format!("authentic, got {outcome:?}").into());
    };
    let waived: Vec<_> = bundle.prior_waivers().fingerprints().collect();
    let fp = *waived.first().ok_or("the decision waived a finding")?;
    assert_eq!(waived.len(), 1);
    assert_eq!(bundle.prior_waivers(), &expected);
    assert_eq!(
        bundle.bindings().digests(fp),
        bundle.prior_waivers().digests(fp),
        "the waived finding is bound on the head as the waiver was"
    );
    Ok(())
}

#[test]
fn a_dispatch_of_the_pipeline_on_the_default_branch_is_authentic() -> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;
    let answer = run_with(&persisted, |run| {
        run.event = RunEvent::WorkflowDispatch;
        "main".clone_into(&mut run.head_branch);
    });

    let (outcome, _) = read(&persisted, answer, stored(&persisted))?;

    assert_matches!(outcome, Ok(EvidenceRead::Authentic(_)));
    Ok(())
}

#[test]
fn a_run_assay_does_not_believe_is_forged_and_nothing_is_fetched() -> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;
    let cases: [(&str, Answer, ProvenanceRefusal); 5] = [
        (
            "a branch's own workflow",
            run_with(&persisted, |run| {
                ".github/workflows/rust.yml".clone_into(&mut run.path);
            }),
            ProvenanceRefusal::Workflow,
        ),
        (
            "a pull_request run",
            run_with(&persisted, |run| run.event = RunEvent::Other),
            ProvenanceRefusal::Event,
        ),
        (
            "attempt 2",
            run_with(&persisted, |run| {
                run.run_attempt = NonZeroU64::new(2).expect("not zero");
            }),
            ProvenanceRefusal::Attempt,
        ),
        (
            "a fork's head",
            run_with(&persisted, |run| run.head_repo = None),
            ProvenanceRefusal::HeadRepository,
        ),
        ("no such run", Answer::NoRun, ProvenanceRefusal::Missing),
    ];
    for (case, answer, refusal) in cases {
        let (outcome, calls) = read(&persisted, answer, stored(&persisted))?;

        assert_matches!(
            outcome,
            Ok(EvidenceRead::Forged(Forgery::Provenance(found))) if found == refusal,
            "{case}"
        );
        assert_eq!(
            calls,
            [Call::Provenance(persisted.entry.pointer.run_id)],
            "{case}: nothing is fetched"
        );
    }
    Ok(())
}

#[test]
fn a_pointer_to_a_re_run_is_forged_before_any_port_call() -> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;
    let pointer = EvidencePointer {
        run_attempt: NonZeroU64::new(2).ok_or("not zero")?,
        ..persisted.entry.pointer.clone()
    };
    let answer = Answer::Run(pipeline_run(&persisted));

    let (outcome, calls) = read_under(
        &persisted,
        &pointer,
        Ports::new(answer, stored(&persisted)),
        &rubric_v1()?,
    );

    assert_matches!(
        outcome,
        Ok(EvidenceRead::Forged(Forgery::Provenance(
            ProvenanceRefusal::Attempt
        )))
    );
    assert_eq!(calls, []);
    Ok(())
}

#[test]
fn a_bundle_that_does_not_hash_to_its_pointer_is_forged() -> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;
    let (bytes, _) = tampered(&persisted, |bundle| {
        bundle["rubric_version"] = Value::from("1 ");
        Ok(())
    })?;
    let answer = Answer::Run(pipeline_run(&persisted));

    let (outcome, calls) = read(&persisted, answer, Stored::Bytes(bytes.clone()))?;

    let Ok(EvidenceRead::Forged(Forgery::Mismatch(Mismatch::Digest { pointer, bundle }))) = outcome
    else {
        return Err(format!("a digest forgery, got {outcome:?}").into());
    };
    assert_eq!(pointer, persisted.entry.pointer.sha256);
    assert_eq!(bundle, Sha256Digest::of(&bytes));
    assert_eq!(calls, read_then_fetched(&persisted));
    Ok(())
}

#[test]
fn a_tampered_document_under_a_re_aimed_pointer_is_forged() -> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;
    let (bytes, pointer) = tampered(&persisted, |bundle| {
        let text = bundle
            .pointer_mut("/documents/0/text")
            .ok_or("the bundle has a document")?;
        *text = Value::from("{}");
        Ok(())
    })?;
    let answer = Answer::Run(pipeline_run(&persisted));

    let (outcome, _) = read_under(
        &persisted,
        &pointer,
        Ports::new(answer, Stored::Bytes(bytes)),
        &rubric_v1()?,
    );

    assert_matches!(
        outcome,
        Ok(EvidenceRead::Forged(Forgery::Mismatch(
            Mismatch::Document { index: 0 }
        )))
    );
    Ok(())
}

#[test]
fn a_tampered_decision_under_a_re_aimed_pointer_is_forged() -> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;
    let (bytes, pointer) = tampered(&persisted, |bundle| {
        let conclusion = bundle
            .pointer_mut("/decision/conclusion")
            .ok_or("the decision has a conclusion")?;
        *conclusion = Value::from(if conclusion == "success" {
            "failure"
        } else {
            "success"
        });
        Ok(())
    })?;
    let answer = Answer::Run(pipeline_run(&persisted));

    let (outcome, _) = read_under(
        &persisted,
        &pointer,
        Ports::new(answer, Stored::Bytes(bytes)),
        &rubric_v1()?,
    );

    assert_matches!(
        outcome,
        Ok(EvidenceRead::Forged(Forgery::Mismatch(Mismatch::Decision(
            _
        ))))
    );
    Ok(())
}

#[test]
fn a_bundle_that_is_not_the_contract_is_forged() -> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;
    let (bytes, pointer) = tampered(&persisted, |bundle| {
        bundle["schema"] = Value::from("assay.run-inputs/2");
        Ok(())
    })?;
    let answer = Answer::Run(pipeline_run(&persisted));

    let (outcome, _) = read_under(
        &persisted,
        &pointer,
        Ports::new(answer, Stored::Bytes(bytes)),
        &rubric_v1()?,
    );

    assert_matches!(
        outcome,
        Ok(EvidenceRead::Forged(Forgery::Shape(BundleShape::Shape)))
    );
    Ok(())
}

#[test]
fn a_bundle_over_the_cap_is_forged() -> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;
    for (case, stored) in [
        ("reported by the store", Stored::TooLarge),
        (
            "one byte over",
            Stored::Bytes(vec![b' '; MAX_BUNDLE_BYTES + 1]),
        ),
    ] {
        let answer = Answer::Run(pipeline_run(&persisted));

        let (outcome, _) = read(&persisted, answer, stored)?;

        assert_matches!(
            outcome,
            Ok(EvidenceRead::Forged(Forgery::TooLarge)),
            "{case}"
        );
    }
    Ok(())
}

#[test]
fn a_missing_or_expired_bundle_is_absent_not_forged() -> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;
    for (stored, absence) in [
        (Stored::Missing, Absence::Missing),
        (Stored::Expired, Absence::Expired),
    ] {
        let answer = Answer::Run(pipeline_run(&persisted));

        let (outcome, _) = read(&persisted, answer, stored)?;

        assert_matches!(outcome, Ok(EvidenceRead::Absent(found)) if found == absence);
    }
    Ok(())
}

#[test]
fn a_bundle_decided_under_another_rubric_version_is_other_rubric() -> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;
    let rubric: Rubric = toml::from_str(
        "version = \"2\"\nmajors_block_threshold = 3\nmax_requeues = 3\nmicro_roles = []\n",
    )?;
    let answer = Answer::Run(pipeline_run(&persisted));

    let (outcome, _) = read_under(
        &persisted,
        &persisted.entry.pointer,
        Ports::new(answer, stored(&persisted)),
        &rubric,
    );

    assert_matches!(outcome, Ok(EvidenceRead::OtherRubric));
    Ok(())
}

#[test]
fn a_port_that_cannot_be_read_is_an_error_not_a_verdict() -> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;

    let (runs, calls) = read(&persisted, Answer::Fails, stored(&persisted))?;
    let answer = Answer::Run(pipeline_run(&persisted));
    let (store, _) = read(&persisted, answer, Stored::Fails)?;

    assert_matches!(runs, Err(EvidenceError::Provenance(FakeError)));
    assert_eq!(calls, [Call::Provenance(persisted.entry.pointer.run_id)]);
    assert_matches!(store, Err(EvidenceError::Fetch(FakeError)));
    Ok(())
}

#[test]
fn the_bundle_is_fetched_from_the_pointer_s_run_under_the_cap() -> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;
    let bundles = FakeBundles::new(stored(&persisted));
    let ports = Ports {
        answer: Answer::Run(pipeline_run(&persisted)),
        bundles,
        calls: RefCell::new(Vec::new()),
    };
    let rubric = rubric_v1()?;
    let request = EvidenceRequest {
        repo_id: persisted.entry.repo_id,
        pointer: &persisted.entry.pointer,
        default_branch: "main",
        rubric: &rubric,
    };

    let outcome = read_evidence(&request, &ports, &ports);

    assert_matches!(outcome, Ok(EvidenceRead::Authentic(_)));
    let run = RunUid {
        repo_id: persisted.entry.repo_id,
        run_id: persisted.entry.pointer.run_id,
        run_attempt: NonZeroU64::MIN,
    };
    assert_eq!(
        *ports.bundles.asked.borrow(),
        [(run, BUNDLE_FILE.to_owned(), MAX_BUNDLE_BYTES)]
    );
    Ok(())
}
