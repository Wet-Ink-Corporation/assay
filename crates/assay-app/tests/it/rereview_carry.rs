//! The carry set across runs (`docs/architecture/04-pipeline.md` §7, #224, owner: bundle): a
//! run's carried and still-open findings go into its `assay.run-inputs/1` bundle, and the next
//! run seeds its carry set from that bundle once it is authenticated by its evidence pointer, so
//! a finding stays open as of its last verification until it is re-judged.

use std::assert_matches;
use std::error::Error;
use std::num::NonZeroU64;

use assay_app::{
    BUNDLE_FILE, BundleShape, CarriedAnchor, CarriedField, CarriedFinding, CarriedProblem,
    CarriedRefusal, Carry, CarryRefused, CarryReset, CarrySet, Change, DiffPaths, EvidencePointer,
    FileDiff, MAX_BUNDLE_BYTES, MAX_CARRIED, Mismatch, OmitReason, Omitted, PriorCarry,
    PriorCarryError, RunField, Sha256Digest, ValidFinding, Verification, VerifierTier, next_carry,
    prior_carry, recheck_anchors, verify,
};
use assay_domain::{Fingerprint, RunUid, Severity, Side, VerifierVerdict};
use serde_json::{Value, json};

use crate::plan_fakes::FakeError;
use crate::publish_support::{Run, stamped};
use crate::replay_fakes::{FakeBundles, Stored};
use crate::verify_fakes::{
    REPOSITORY, Scripted, Unreachable, bundle, carried, finding, request_carrying, verdict,
};

/// `(fp, severity, verdict)` of each finding the set carries, in order.
fn carried_out(set: &CarrySet) -> Vec<(Fingerprint, Severity, Option<VerifierVerdict>)> {
    set.findings()
        .iter()
        .map(|found| (found.fp(), found.severity(), found.verdict()))
        .collect()
}

/// Verifies `carried` and `raised` in `tier` with `runner`, then derives the next carry set.
fn carried_on(
    tier: VerifierTier,
    carried: &[CarriedFinding],
    raised: &[ValidFinding],
    runner: &Scripted,
) -> Result<(CarrySet, Verification<Unreachable>), Box<dyn Error>> {
    let bundle = bundle()?;
    let verification = verify(runner, request_carrying(tier, raised, carried, &bundle));
    Ok((
        next_carry(carried.to_vec(), raised, &verification, REPOSITORY),
        verification,
    ))
}

/// A fresh finding of `severity` fingerprinted `n`.
fn fresh(n: u64, severity: Severity) -> Result<ValidFinding, Box<dyn Error>> {
    let index = usize::try_from(n)?;
    let mut found = finding(index, severity)?;
    found.fingerprint = fp(n)?;
    Ok(found)
}

/// The fingerprint `n` in 16 hex digits.
fn fp(n: u64) -> Result<Fingerprint, Box<dyn Error>> {
    Ok(format!("{n:016x}").parse()?)
}

#[test]
fn a_refuted_carried_finding_leaves_the_carry_set() -> Result<(), Box<dyn Error>> {
    let carried = [carried(0, Severity::Blocker)?];
    let runner = Scripted::answering([(verdict("refuted", "the guard is in"), 1)]);

    let (set, _) = carried_on(VerifierTier::Lite, &carried, &[], &runner)?;

    assert_eq!(carried_out(&set), []);
    assert_eq!(set.omitted(), []);
    Ok(())
}

#[test]
fn a_confirmed_or_unclear_carried_finding_carries_this_runs_verdict() -> Result<(), Box<dyn Error>>
{
    let carried = [
        carried(0, Severity::Blocker)?,
        carried(1, Severity::Blocker)?,
    ];
    let runner = Scripted::answering([
        (verdict("confirmed", "still unguarded"), 1),
        (verdict("unclear", "could not tell"), 1),
    ]);

    let (set, _) = carried_on(VerifierTier::Lite, &carried, &[], &runner)?;

    assert_eq!(
        carried_out(&set),
        [
            (fp(0)?, Severity::Blocker, Some(VerifierVerdict::Confirmed)),
            (fp(1)?, Severity::Major, Some(VerifierVerdict::Unclear)),
        ]
    );
    Ok(())
}

#[test]
fn a_carried_finding_not_re_verified_keeps_its_last_severity_and_verdict()
-> Result<(), Box<dyn Error>> {
    let first = [carried(0, Severity::Blocker)?];
    let confirming = Scripted::answering([(verdict("confirmed", "still unguarded"), 1)]);
    let (judged, _) = carried_on(VerifierTier::Lite, &first, &[], &confirming)?;
    let failing = Scripted::answering([(assay_app::ModelOutcome::Timeout, 1)]);

    let (set, _) = carried_on(VerifierTier::Lite, judged.findings(), &[], &failing)?;

    assert_eq!(
        carried_out(&set),
        [(fp(0)?, Severity::Blocker, Some(VerifierVerdict::Confirmed))]
    );
    Ok(())
}

#[test]
fn a_carried_finding_past_the_cap_stays_as_last_verified() -> Result<(), Box<dyn Error>> {
    let carried = (0..4)
        .map(|n| carried(n, Severity::Blocker))
        .collect::<Result<Vec<_>, _>>()?;
    let runner = Scripted::answering((0..3).map(|_| (verdict("confirmed", "seen"), 1)));

    let (set, _) = carried_on(VerifierTier::Lite, &carried, &[], &runner)?;

    assert_eq!(
        carried_out(&set).last(),
        Some(&(fp(3)?, Severity::Blocker, None))
    );
    assert_eq!(set.findings().len(), 4);
    Ok(())
}

#[test]
fn micro_carries_every_carried_finding_as_last_verified() -> Result<(), Box<dyn Error>> {
    let carried = [carried(0, Severity::Major)?];
    let runner = Scripted::new([]);

    let (set, _) = carried_on(VerifierTier::Micro, &carried, &[], &runner)?;

    assert_eq!(carried_out(&set), [(fp(0)?, Severity::Major, None)]);
    Ok(())
}

#[test]
fn a_carried_finding_whose_evidence_moved_is_carried_where_it_is_now() -> Result<(), Box<dyn Error>>
{
    let change = Change {
        head_files: [(
            "src/moved.rs".to_owned(),
            "fn f() {\n}\n\nlet x = input.parse().unwrap();\n".to_owned(),
        )]
        .into_iter()
        .collect(),
        ..Change::default()
    };
    let rechecked = recheck_anchors(vec![carried(0, Severity::Major)?], &change);
    let runner = Scripted::new([]);

    let (set, _) = carried_on(VerifierTier::Micro, &rechecked, &[], &runner)?;

    let lines: Vec<(&str, u64)> = set
        .findings()
        .iter()
        .map(|found| (found.file(), found.line().get()))
        .collect();
    assert_eq!(lines, [("src/moved.rs", 4)]);
    Ok(())
}

#[test]
fn a_fresh_finding_this_run_kept_joins_the_carry_set_at_its_head_location()
-> Result<(), Box<dyn Error>> {
    let raised = [fresh(7, Severity::Minor)?];
    let runner = Scripted::new([]);

    let (set, _) = carried_on(VerifierTier::Lite, &[], &raised, &runner)?;

    let [found] = set.findings() else {
        return Err(format!("one carried finding, got {:?}", set.findings()).into());
    };
    assert_eq!(
        (
            found.fp(),
            found.file(),
            found.line().get(),
            found.severity(),
            found.verdict(),
            found.title(),
            found.evidence(),
        ),
        (
            fp(7)?,
            "src/lib.rs",
            12,
            Severity::Minor,
            None,
            "unwrap on user input panics (7)",
            "let x = input.parse().unwrap();",
        )
    );
    Ok(())
}

#[test]
fn a_fresh_finding_the_verifier_refuted_is_not_carried() -> Result<(), Box<dyn Error>> {
    let raised = [fresh(1, Severity::Blocker)?, fresh(2, Severity::Blocker)?];
    let runner = Scripted::answering([
        (verdict("refuted", "not reachable"), 1),
        (verdict("confirmed", "reachable"), 1),
    ]);

    let (set, _) = carried_on(VerifierTier::Lite, &[], &raised, &runner)?;

    assert_eq!(
        carried_out(&set),
        [(fp(2)?, Severity::Blocker, Some(VerifierVerdict::Confirmed))]
    );
    Ok(())
}

#[test]
fn a_carried_fingerprint_raised_again_is_carried_once_from_the_fresh_finding()
-> Result<(), Box<dyn Error>> {
    let carried = [carried(0, Severity::Blocker)?];
    let mut again = fresh(0, Severity::Blocker)?;
    again.fingerprint = carried[0].fp();
    let runner = Scripted::answering([
        (verdict("refuted", "looks fixed"), 1),
        (verdict("confirmed", "still unguarded"), 1),
    ]);

    let (set, _) = carried_on(VerifierTier::Full, &carried, &[again], &runner)?;

    let shown: Vec<(Fingerprint, &str, Severity)> = set
        .findings()
        .iter()
        .map(|found| (found.fp(), found.title(), found.severity()))
        .collect();
    assert_eq!(
        shown,
        [(fp(0)?, "unwrap on user input panics (0)", Severity::Blocker)]
    );
    Ok(())
}

#[test]
fn a_carried_finding_raised_again_lower_keeps_its_higher_severity() -> Result<(), Box<dyn Error>> {
    let carried = [carried(0, Severity::Blocker)?];
    let mut again = fresh(0, Severity::Nit)?;
    again.fingerprint = carried[0].fp();
    let runner = Scripted::answering([(verdict("confirmed", "still unguarded"), 1)]);

    let (set, _) = carried_on(VerifierTier::Lite, &carried, &[again], &runner)?;

    assert_eq!(
        carried_out(&set),
        [(fp(0)?, Severity::Blocker, Some(VerifierVerdict::Confirmed))]
    );
    Ok(())
}

#[test]
fn the_carry_set_is_capped_most_severe_first_and_lists_the_rest_as_omitted()
-> Result<(), Box<dyn Error>> {
    let total = MAX_CARRIED + 2;
    let raised = (0..total)
        .map(|n| {
            let severity = if n == total - 1 {
                Severity::Major
            } else {
                Severity::Nit
            };
            fresh(u64::try_from(n)?, severity)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let runner = Scripted::answering([(verdict("confirmed", "reachable"), 1)]);

    let (set, _) = carried_on(VerifierTier::Lite, &[], &raised, &runner)?;

    assert_eq!(set.findings().len(), MAX_CARRIED);
    assert_eq!(
        set.findings().first().map(CarriedFinding::fp),
        Some(fp(u64::try_from(total - 1)?)?)
    );
    let cut = u64::try_from(MAX_CARRIED - 1)?;
    assert_eq!(
        set.omitted(),
        [
            Omitted {
                fp: fp(cut)?,
                reason: OmitReason::Cap
            },
            Omitted {
                fp: fp(cut + 1)?,
                reason: OmitReason::Cap
            },
        ]
    );
    Ok(())
}

#[test]
fn a_fresh_finding_the_carried_intake_would_refuse_is_listed_as_omitted()
-> Result<(), Box<dyn Error>> {
    let mut odd = fresh(3, Severity::Minor)?;
    odd.finding.title = "a title\nwith a line break".to_owned();
    let runner = Scripted::new([]);

    let (set, _) = carried_on(VerifierTier::Lite, &[], &[odd], &runner)?;

    assert_eq!(carried_out(&set), []);
    assert_eq!(
        set.omitted(),
        [Omitted {
            fp: fp(3)?,
            reason: OmitReason::Refused(CarriedRefusal {
                field: CarriedField::Title,
                problem: CarriedProblem::Control,
            }),
        }]
    );
    Ok(())
}

#[test]
fn an_unjudged_carry_set_keeps_every_finding_as_last_verified() -> Result<(), Box<dyn Error>> {
    let carried = vec![carried(0, Severity::Minor)?, carried(1, Severity::Blocker)?];

    let set = CarrySet::unjudged(carried);

    assert_eq!(
        carried_out(&set),
        [
            (fp(1)?, Severity::Blocker, None),
            (fp(0)?, Severity::Minor, None),
        ]
    );
    Ok(())
}

/// A published run carrying `set`: its bundle's bytes and the pointer to them.
pub(crate) fn published(set: CarrySet) -> Result<(Run, Vec<u8>, EvidencePointer), Box<dyn Error>> {
    let mut run = Run::new()?;
    run.carry = set;
    let world = stamped()?;
    run.publish(&world)?;
    let bytes = {
        let uploads = world.uploads.borrow();
        let [(_, file, bytes)] = uploads.as_slice() else {
            return Err(format!("expected one upload, got {}", uploads.len()).into());
        };
        assert_eq!(file, BUNDLE_FILE, "the bundle file");
        bytes.clone()
    };
    let pointer = EvidencePointer {
        run_id: run.request.plan.run_uid.run_id,
        run_attempt: NonZeroU64::MIN,
        sha256: Sha256Digest::of(&bytes),
    };
    Ok((run, bytes, pointer))
}

/// What `prior_carry` reads for `run`'s repository through `pointer` from `stored`.
fn prior(
    run: &Run,
    carry: Carry,
    pointer: &EvidencePointer,
    stored: Stored,
) -> (
    Result<Vec<CarriedFinding>, PriorCarryError<FakeError>>,
    FakeBundles,
) {
    let source = FakeBundles::new(stored);
    let request = PriorCarry {
        repo_id: run.request.plan.run_uid.repo_id,
        pointer,
        repository: REPOSITORY,
    };
    (prior_carry(carry, &request, &source), source)
}

/// Two carried findings, one with a verdict and a line range.
pub(crate) fn two_carried() -> Result<CarrySet, Box<dyn Error>> {
    let first = [carried(0, Severity::Blocker)?, carried(1, Severity::Minor)?];
    let runner = Scripted::answering([
        (verdict("confirmed", "still unguarded"), 1),
        (verdict("unclear", "could not tell"), 1),
    ]);
    Ok(carried_on(VerifierTier::Lite, &first, &[], &runner)?.0)
}

#[test]
fn the_next_run_reads_the_carry_set_its_predecessor_persisted() -> Result<(), Box<dyn Error>> {
    let set = two_carried()?;
    let (run, bytes, pointer) = published(set.clone())?;

    let (read, source) = prior(&run, Carry::Kept, &pointer, Stored::Bytes(bytes));

    assert_eq!(read?, set.findings());
    assert_eq!(
        source.asked.borrow().as_slice(),
        [(
            RunUid {
                repo_id: run.request.plan.run_uid.repo_id,
                run_id: pointer.run_id,
                run_attempt: NonZeroU64::MIN,
            },
            BUNDLE_FILE.to_owned(),
            MAX_BUNDLE_BYTES
        )]
    );
    Ok(())
}

#[test]
fn a_line_range_and_a_missing_trigger_survive_the_round_trip() -> Result<(), Box<dyn Error>> {
    let mut spanning = fresh(5, Severity::Minor)?;
    spanning.finding.end_line = NonZeroU64::new(14);
    spanning.finding.trigger = None;
    let runner = Scripted::new([]);
    let (set, _) = carried_on(VerifierTier::Lite, &[], &[spanning], &runner)?;
    let (run, bytes, pointer) = published(set)?;

    let (read, _) = prior(&run, Carry::Kept, &pointer, Stored::Bytes(bytes));

    let read = read?;
    let ranges: Vec<(u64, Option<u64>, Option<&str>)> = read
        .iter()
        .map(|found| {
            (
                found.line().get(),
                found.end_line().map(NonZeroU64::get),
                found.trigger(),
            )
        })
        .collect();
    assert_eq!(ranges, [(12, Some(14), None)]);
    Ok(())
}

#[test]
fn a_reset_or_first_review_carries_nothing_and_fetches_nothing() -> Result<(), Box<dyn Error>> {
    let (run, bytes, pointer) = published(two_carried()?)?;

    for carry in [
        Carry::FirstReview,
        Carry::Reset(CarryReset::Rewritten),
        Carry::Reset(CarryReset::Unreachable),
    ] {
        let (read, source) = prior(&run, carry, &pointer, Stored::Bytes(bytes.clone()));

        assert_eq!(read?, [], "{carry:?} carries nothing");
        assert_eq!(source.asked.borrow().len(), 0, "{carry:?} fetches nothing");
    }
    Ok(())
}

#[test]
fn a_bundle_that_does_not_hash_to_its_pointer_seeds_nothing() -> Result<(), Box<dyn Error>> {
    let (run, mut bytes, pointer) = published(two_carried()?)?;
    bytes.push(b' ');

    let (read, _) = prior(&run, Carry::Kept, &pointer, Stored::Bytes(bytes));

    assert_matches!(read, Err(PriorCarryError::Digest));
    Ok(())
}

#[test]
fn a_pointer_to_a_re_run_is_refused_before_any_fetch() -> Result<(), Box<dyn Error>> {
    let (run, bytes, mut pointer) = published(two_carried()?)?;
    pointer.run_attempt = NonZeroU64::new(2).ok_or("2 is not zero")?;

    let (read, source) = prior(&run, Carry::Kept, &pointer, Stored::Bytes(bytes));

    assert_matches!(read, Err(PriorCarryError::NotFirstAttempt));
    assert_eq!(source.asked.borrow().len(), 0);
    Ok(())
}

#[test]
fn a_missing_expired_or_oversized_bundle_is_an_error() -> Result<(), Box<dyn Error>> {
    let (run, _, pointer) = published(two_carried()?)?;

    let (missing, _) = prior(&run, Carry::Kept, &pointer, Stored::Missing);
    let (expired, _) = prior(&run, Carry::Kept, &pointer, Stored::Expired);
    let (large, _) = prior(&run, Carry::Kept, &pointer, Stored::TooLarge);
    let (fails, _) = prior(&run, Carry::Kept, &pointer, Stored::Fails);

    assert_matches!(missing, Err(PriorCarryError::Missing));
    assert_matches!(expired, Err(PriorCarryError::Expired));
    assert_matches!(large, Err(PriorCarryError::TooLarge));
    assert_matches!(fails, Err(PriorCarryError::Fetch(FakeError)));
    Ok(())
}

/// `bytes` with `edit` applied to its parsed JSON, and a pointer to the result.
fn rewritten(
    bytes: &[u8],
    pointer: &EvidencePointer,
    edit: impl FnOnce(&mut Value) -> Result<(), Box<dyn Error>>,
) -> Result<(Vec<u8>, EvidencePointer), Box<dyn Error>> {
    let mut bundle: Value = serde_json::from_slice(bytes)?;
    edit(&mut bundle)?;
    let bytes = serde_json::to_vec(&bundle)?;
    let pointer = EvidencePointer {
        sha256: Sha256Digest::of(&bytes),
        ..pointer.clone()
    };
    Ok((bytes, pointer))
}

#[test]
fn a_carried_record_the_intake_refuses_refuses_the_set() -> Result<(), Box<dyn Error>> {
    let (run, bytes, pointer) = published(two_carried()?)?;
    let (bytes, pointer) = rewritten(&bytes, &pointer, |bundle| {
        *bundle
            .pointer_mut("/carry/findings/1/file")
            .ok_or("a second record")? = json!("../outside.rs");
        Ok(())
    })?;

    let (read, _) = prior(&run, Carry::Kept, &pointer, Stored::Bytes(bytes));

    assert_matches!(
        read,
        Err(PriorCarryError::Refused(CarryRefused::Finding {
            index: 1,
            refusal: CarriedRefusal {
                field: CarriedField::File,
                problem: CarriedProblem::Segment,
            },
        }))
    );
    Ok(())
}

#[test]
fn a_carried_record_with_an_unknown_field_is_not_the_contract() -> Result<(), Box<dyn Error>> {
    let (run, bytes, pointer) = published(two_carried()?)?;
    let (bytes, pointer) = rewritten(&bytes, &pointer, |bundle| {
        bundle
            .pointer_mut("/carry/findings/0")
            .and_then(Value::as_object_mut)
            .ok_or("a first record")?
            .insert("body".to_owned(), json!("instructions"));
        Ok(())
    })?;

    let (read, _) = prior(&run, Carry::Kept, &pointer, Stored::Bytes(bytes));

    assert_matches!(read, Err(PriorCarryError::Shape(BundleShape::Shape)));
    Ok(())
}

#[test]
fn a_bundle_naming_another_run_is_inconsistent() -> Result<(), Box<dyn Error>> {
    let (run, bytes, pointer) = published(two_carried()?)?;
    let (bytes, pointer) = rewritten(&bytes, &pointer, |bundle| {
        *bundle.pointer_mut("/run_uid/run_id").ok_or("a run id")? = json!(8);
        Ok(())
    })?;

    let (read, _) = prior(&run, Carry::Kept, &pointer, Stored::Bytes(bytes));

    assert_matches!(
        read,
        Err(PriorCarryError::Inconsistent(Mismatch::Run(
            RunField::RunId
        )))
    );
    Ok(())
}

/// Review F5: a store that hands back more than the cap is refused, whatever it claims.
#[test]
fn a_bundle_found_over_the_cap_is_too_large() -> Result<(), Box<dyn Error>> {
    let (run, _, pointer) = published(two_carried()?)?;
    let oversized = vec![b' '; MAX_BUNDLE_BYTES + 1];

    let (read, _) = prior(&run, Carry::Kept, &pointer, Stored::Bytes(oversized));

    assert_matches!(read, Err(PriorCarryError::TooLarge));
    Ok(())
}

/// Review F1: a carried finding whose file was renamed to a path the intake refuses stays
/// where it was published, so the next run still reads the whole set.
#[test]
fn a_rename_to_a_refused_path_keeps_the_published_location() -> Result<(), Box<dyn Error>> {
    let hidden = "src/a\u{200b}.rs";
    let change = Change {
        head_files: [(
            hidden.to_owned(),
            "let x = input.parse().unwrap();\n".to_owned(),
        )]
        .into_iter()
        .collect(),
        diffs: vec![FileDiff {
            paths: DiffPaths::Renamed {
                base: "src/moved.rs".to_owned(),
                head: hidden.to_owned(),
            },
            hunks: Vec::new(),
        }],
        ..Change::default()
    };
    let rechecked = recheck_anchors(
        vec![carried(0, Severity::Major)?, carried(1, Severity::Minor)?],
        &change,
    );
    let runner = Scripted::new([]);
    let (set, _) = carried_on(VerifierTier::Micro, &rechecked, &[], &runner)?;
    let (run, bytes, pointer) = published(set)?;

    let (read, _) = prior(&run, Carry::Kept, &pointer, Stored::Bytes(bytes));

    let placed: Vec<(CarriedAnchor, &str, u64)> = rechecked
        .iter()
        .map(|found| (found.anchor(), found.file(), found.line().get()))
        .collect();
    assert_eq!(
        placed,
        [
            (CarriedAnchor::Gone, "src/moved.rs", 40),
            (CarriedAnchor::Gone, "src/moved.rs", 40)
        ]
    );
    let files: Vec<String> = read?.iter().map(|found| found.file().to_owned()).collect();
    assert_eq!(files, ["src/moved.rs", "src/moved.rs"]);
    Ok(())
}

/// Review F2: a finding on deleted code has no location at head, so it is not carried.
#[test]
fn a_fresh_finding_on_the_deleted_side_is_omitted_not_carried() -> Result<(), Box<dyn Error>> {
    let mut deleted = fresh(4, Severity::Major)?;
    deleted.finding.side = Some(Side::Left);
    let mut kept = fresh(5, Severity::Minor)?;
    kept.finding.side = Some(Side::Right);
    let runner = Scripted::answering([(verdict("confirmed", "reachable"), 1)]);

    let (set, _) = carried_on(VerifierTier::Lite, &[], &[deleted, kept], &runner)?;

    assert_eq!(carried_out(&set), [(fp(5)?, Severity::Minor, None)]);
    assert_eq!(
        set.omitted(),
        [Omitted {
            fp: fp(4)?,
            reason: OmitReason::DeletedSide,
        }]
    );
    Ok(())
}

/// Review F3: a re-raise at the same severity keeps this run's verdict on the carried claim.
#[test]
fn a_re_raise_at_the_same_severity_keeps_the_carried_verdict() -> Result<(), Box<dyn Error>> {
    let carried = [carried(0, Severity::Minor)?];
    let mut again = fresh(0, Severity::Minor)?;
    again.fingerprint = carried[0].fp();
    let runner = Scripted::answering([(verdict("confirmed", "still unguarded"), 1)]);

    let (set, _) = carried_on(VerifierTier::Lite, &carried, &[again], &runner)?;

    assert_eq!(
        carried_out(&set),
        [(fp(0)?, Severity::Minor, Some(VerifierVerdict::Confirmed))]
    );
    Ok(())
}

/// Review F5: one fingerprint raised twice is carried once, the later text at the higher
/// severity.
#[test]
fn a_fingerprint_raised_twice_is_carried_once_at_the_higher_severity() -> Result<(), Box<dyn Error>>
{
    let first = fresh(6, Severity::Major)?;
    let mut second = fresh(7, Severity::Nit)?;
    second.fingerprint = first.fingerprint;
    let runner = Scripted::answering([(verdict("confirmed", "reachable"), 1)]);

    let (set, _) = carried_on(VerifierTier::Lite, &[], &[first, second], &runner)?;

    let shown: Vec<(Fingerprint, &str, Severity, Option<VerifierVerdict>)> = set
        .findings()
        .iter()
        .map(|found| (found.fp(), found.title(), found.severity(), found.verdict()))
        .collect();
    assert_eq!(
        shown,
        [(
            fp(6)?,
            "unwrap on user input panics (7)",
            Severity::Major,
            Some(VerifierVerdict::Confirmed)
        )]
    );
    Ok(())
}

/// Review F5: a lower re-raise of a carried finding that was not re-verified keeps its last
/// severity, with the fresh text.
#[test]
fn a_lower_re_raise_of_an_unverified_carried_finding_keeps_its_severity()
-> Result<(), Box<dyn Error>> {
    let carried = [carried(0, Severity::Blocker)?];
    let mut again = fresh(0, Severity::Nit)?;
    again.fingerprint = carried[0].fp();
    let runner = Scripted::new([]);

    let (set, _) = carried_on(VerifierTier::Micro, &carried, &[again], &runner)?;

    let shown: Vec<(&str, Severity, Option<VerifierVerdict>)> = set
        .findings()
        .iter()
        .map(|found| (found.title(), found.severity(), found.verdict()))
        .collect();
    assert_eq!(
        shown,
        [("unwrap on user input panics (0)", Severity::Blocker, None)]
    );
    Ok(())
}
