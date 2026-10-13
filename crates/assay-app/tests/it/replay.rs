//! Decision replay (R-09, ADR-15, `docs/architecture/04-pipeline.md` §8): the persisted
//! `RunInputs` bundle, read through its evidence pointer, recomputes to the recorded decision
//! byte for byte; a tampered bundle is a mismatch; an expired, missing, over-cap or unreadable
//! one is an error, never a pass (wi-b5affa).

use std::assert_matches;
use std::error::Error;
use std::num::NonZeroU64;

use assay_app::{
    BUNDLE_FILE, BundleShape, DecisionDiff, EvidencePointer, MAX_BUNDLE_BYTES, Mismatch,
    ModelReplay, ReplayError, ReplayRequest, Replayed, RunField, Sha256Digest, replay_decision,
};
use assay_domain::{Conclusion, Rubric, RunUid};
use serde_json::{Value, json};

use crate::decide_fakes::Scenario;
use crate::plan_fakes::FakeError;
use crate::replay_fakes::{FakeBundles, Persisted, Stored};
use crate::support::rubric_v1;

/// What `replay_decision` returns over the fake.
type Outcome = Result<Replayed, ReplayError<FakeError>>;

/// Replays `pointer` of the persisted run's repository against `stored` under rubric v1.
fn replay(
    persisted: &Persisted,
    pointer: &EvidencePointer,
    stored: Stored,
) -> Result<(Outcome, FakeBundles), Box<dyn Error>> {
    Ok(replay_under(persisted, pointer, stored, &rubric_v1()?))
}

/// Replays `pointer` against `stored` under `rubric`.
fn replay_under(
    persisted: &Persisted,
    pointer: &EvidencePointer,
    stored: Stored,
    rubric: &Rubric,
) -> (Outcome, FakeBundles) {
    let source = FakeBundles::new(stored);
    let request = ReplayRequest {
        repo_id: persisted.entry.repo_id,
        pointer,
        rubric,
    };
    let outcome = replay_decision(&request, &source);
    (outcome, source)
}

/// The persisted bundle with `tamper` applied to its parsed JSON, written back.
fn tampered(
    persisted: &Persisted,
    tamper: impl FnOnce(&mut Value) -> Result<(), Box<dyn Error>>,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut bundle: Value = serde_json::from_slice(&persisted.bytes)?;
    tamper(&mut bundle)?;
    Ok(serde_json::to_vec(&bundle)?)
}

/// The persisted pointer re-aimed at `bytes`, as a writer of the ledger would re-point it.
fn repointed(persisted: &Persisted, bytes: &[u8]) -> EvidencePointer {
    EvidencePointer {
        sha256: Sha256Digest::of(bytes),
        ..persisted.entry.pointer.clone()
    }
}

/// `bytes` stored under a pointer re-aimed at them.
fn replay_repointed(persisted: &Persisted, bytes: Vec<u8>) -> Result<Outcome, Box<dyn Error>> {
    let pointer = repointed(persisted, &bytes);
    Ok(replay(persisted, &pointer, Stored::Bytes(bytes))?.0)
}

/// The JSON at `pointer` in `bundle`, for a tamper to overwrite.
fn at<'v>(bundle: &'v mut Value, pointer: &str) -> Result<&'v mut Value, Box<dyn Error>> {
    bundle
        .pointer_mut(pointer)
        .ok_or_else(|| format!("the bundle has no {pointer}").into())
}

/// The offset just past `needle` in the canonical (compact, sorted) encoding of the bundle's
/// recorded decision.
fn offset_after(bundle: &[u8], needle: &str) -> Result<usize, Box<dyn Error>> {
    let parsed: Value = serde_json::from_slice(bundle)?;
    let decision = serde_json::to_vec(parsed.get("decision").ok_or("no decision")?)?;
    let start = decision
        .windows(needle.len())
        .position(|window| window == needle.as_bytes())
        .ok_or_else(|| format!("{needle} is not in the decision"))?;
    Ok(start + needle.len())
}

#[test]
fn a_fresh_bundle_replays_to_its_recorded_decision() -> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;
    let pointer = persisted.entry.pointer.clone();

    let (outcome, source) = replay(&persisted, &pointer, Stored::Bytes(persisted.bytes.clone()))?;

    let Replayed::Match(matched) = outcome? else {
        return Err("a fresh bundle must match".into());
    };
    assert_eq!(matched.decision.conclusion, Conclusion::Success);
    assert_eq!(matched.decision.key, persisted.run.inputs.decision_key);
    assert_eq!(
        matched.model,
        ModelReplay::RecordedOnly {
            review_documents: 0
        }
    );
    let run = RunUid {
        repo_id: persisted.entry.repo_id,
        run_id: pointer.run_id,
        run_attempt: pointer.run_attempt,
    };
    assert_eq!(
        *source.asked.borrow(),
        [(run, BUNDLE_FILE.to_owned(), MAX_BUNDLE_BYTES)],
        "one fetch of the pointer's run, under the bundle cap"
    );
    Ok(())
}

#[test]
fn recorded_model_outputs_are_counted_and_never_re_run() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.publish_review("combined-micro.json", b"{}".to_vec())?;
    let persisted = Persisted::of(&scenario)?;

    let (outcome, _) = replay(
        &persisted,
        &persisted.entry.pointer,
        Stored::Bytes(persisted.bytes.clone()),
    )?;

    assert_matches!(
        outcome?,
        Replayed::Match(matched) if matched.model == ModelReplay::RecordedOnly { review_documents: 1 }
    );
    Ok(())
}

#[test]
fn tampered_inputs_under_the_recorded_pointer_fail_its_digest() -> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;
    let bytes = tampered(&persisted, |bundle| {
        *at(bundle, "/inputs/pr/requeues")? = json!(1);
        Ok(())
    })?;
    let bundle = Sha256Digest::of(&bytes);

    let (outcome, _) = replay(&persisted, &persisted.entry.pointer, Stored::Bytes(bytes))?;

    assert_eq!(
        outcome?,
        Replayed::Mismatch(Mismatch::Digest {
            pointer: persisted.entry.pointer.sha256,
            bundle,
        })
    );
    Ok(())
}

#[test]
fn tampered_inputs_under_a_re_aimed_pointer_recompute_to_another_decision()
-> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;
    let bytes = tampered(&persisted, |bundle| {
        *at(bundle, "/inputs/pr/requeues")? = json!(1);
        Ok(())
    })?;
    let hex = offset_after(&bytes, "\"inputs_digest\":\"sha256:")?;

    let outcome = replay_repointed(&persisted, bytes)?;

    // The two digests part somewhere in their 64 hex digits.
    assert_matches!(
        outcome?,
        Replayed::Mismatch(Mismatch::Decision(DecisionDiff { offset, field }))
            if field == "/inputs_digest" && (hex..hex + 64).contains(&offset)
    );
    Ok(())
}

#[test]
fn a_tampered_decision_is_a_mismatch_at_its_first_differing_field() -> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;
    let bytes = tampered(&persisted, |bundle| {
        *at(bundle, "/decision/conclusion")? = json!("failure");
        Ok(())
    })?;
    let offset = offset_after(&bytes, "\"conclusion\":\"")?;

    let outcome = replay_repointed(&persisted, bytes)?;

    assert_eq!(
        outcome?,
        Replayed::Mismatch(Mismatch::Decision(DecisionDiff {
            offset,
            field: "/conclusion".to_owned(),
        }))
    );
    Ok(())
}

#[test]
fn a_key_only_the_recorded_decision_has_is_named_without_echoing_it() -> Result<(), Box<dyn Error>>
{
    let persisted = Persisted::new()?;
    let bytes = tampered(&persisted, |bundle| {
        let decision = at(bundle, "/decision")?
            .as_object_mut()
            .ok_or("the decision is an object")?;
        decision.insert("zz <img src=x>".to_owned(), json!(1));
        Ok(())
    })?;
    // The extra key sorts last, so the encodings part where the replayed one closes its object.
    let offset = offset_after(&bytes, "\"waived\":[]")?;

    let outcome = replay_repointed(&persisted, bytes)?;

    let Replayed::Mismatch(mismatch) = outcome? else {
        return Err("an extra key must not match".into());
    };
    assert_eq!(
        mismatch,
        Mismatch::Decision(DecisionDiff {
            offset,
            field: "/*".to_owned(),
        })
    );
    assert!(
        !mismatch.to_string().contains("img"),
        "the report must not quote the bundle: {mismatch}"
    );
    Ok(())
}

#[test]
fn a_tampered_pointer_digest_is_a_mismatch() -> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;
    let forged: Sha256Digest = format!("sha256:{}", "0".repeat(64)).parse()?;
    let pointer = EvidencePointer {
        sha256: forged.clone(),
        ..persisted.entry.pointer
    };

    let (outcome, _) = replay(&persisted, &pointer, Stored::Bytes(persisted.bytes.clone()))?;

    assert_eq!(
        outcome?,
        Replayed::Mismatch(Mismatch::Digest {
            pointer: forged,
            bundle: Sha256Digest::of(&persisted.bytes),
        })
    );
    Ok(())
}

#[test]
fn a_pointer_to_another_run_is_a_mismatch_of_the_run() -> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;
    let pointer = EvidencePointer {
        run_id: NonZeroU64::new(8).ok_or("not zero")?,
        ..persisted.entry.pointer.clone()
    };

    let (outcome, _) = replay(&persisted, &pointer, Stored::Bytes(persisted.bytes.clone()))?;

    assert_eq!(outcome?, Replayed::Mismatch(Mismatch::Run(RunField::RunId)));
    Ok(())
}

#[test]
fn a_bundle_of_another_repository_is_a_mismatch_of_the_run() -> Result<(), Box<dyn Error>> {
    let mut persisted = Persisted::new()?;
    persisted.entry.repo_id = NonZeroU64::new(1).ok_or("not zero")?;

    let (outcome, _) = replay(
        &persisted,
        &persisted.entry.pointer,
        Stored::Bytes(persisted.bytes.clone()),
    )?;

    assert_eq!(
        outcome?,
        Replayed::Mismatch(Mismatch::Run(RunField::RepoId))
    );
    Ok(())
}

#[test]
fn a_bundle_recording_a_re_run_is_a_mismatch_of_the_run() -> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;
    let bytes = tampered(&persisted, |bundle| {
        *at(bundle, "/run_uid/run_attempt")? = json!(2);
        Ok(())
    })?;

    let outcome = replay_repointed(&persisted, bytes)?;

    assert_eq!(
        outcome?,
        Replayed::Mismatch(Mismatch::Run(RunField::RunAttempt))
    );
    Ok(())
}

#[test]
fn a_document_that_no_longer_hashes_to_its_digest_is_a_mismatch() -> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;
    let bytes = tampered(&persisted, |bundle| {
        *at(bundle, "/documents/1/text")? = json!("{}");
        Ok(())
    })?;

    let outcome = replay_repointed(&persisted, bytes)?;

    assert_eq!(
        outcome?,
        Replayed::Mismatch(Mismatch::Document { index: 1 })
    );
    Ok(())
}

#[test]
fn an_evidence_key_other_than_the_inputs_is_a_mismatch() -> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;
    let bytes = tampered(&persisted, |bundle| {
        *at(bundle, "/evidence_key")? = json!("sha256:other");
        Ok(())
    })?;

    let outcome = replay_repointed(&persisted, bytes)?;

    assert_eq!(outcome?, Replayed::Mismatch(Mismatch::EvidenceKey));
    Ok(())
}

#[test]
fn a_missing_bundle_is_an_error() -> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;

    let (outcome, _) = replay(&persisted, &persisted.entry.pointer, Stored::Missing)?;

    assert_matches!(outcome, Err(ReplayError::Missing));
    Ok(())
}

#[test]
fn an_expired_bundle_is_an_error() -> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;

    let (outcome, _) = replay(&persisted, &persisted.entry.pointer, Stored::Expired)?;

    assert_matches!(outcome, Err(ReplayError::Expired));
    Ok(())
}

#[test]
fn a_store_that_cannot_be_read_is_an_error() -> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;

    let (outcome, _) = replay(&persisted, &persisted.entry.pointer, Stored::Fails)?;

    assert_matches!(outcome, Err(ReplayError::Fetch(FakeError)));
    Ok(())
}

#[test]
fn a_bundle_the_store_reports_over_the_cap_is_an_error() -> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;

    let (outcome, _) = replay(&persisted, &persisted.entry.pointer, Stored::TooLarge)?;

    assert_matches!(outcome, Err(ReplayError::TooLarge));
    Ok(())
}

#[test]
fn a_bundle_one_byte_over_the_cap_is_refused_unread() -> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;
    let over = vec![b' '; MAX_BUNDLE_BYTES + 1];

    let (outcome, _) = replay(&persisted, &persisted.entry.pointer, Stored::Bytes(over))?;

    assert_matches!(outcome, Err(ReplayError::TooLarge));
    Ok(())
}

#[test]
fn a_bundle_of_exactly_the_cap_is_read_and_authenticated() -> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;
    let full = vec![b' '; MAX_BUNDLE_BYTES];

    let (outcome, _) = replay(&persisted, &persisted.entry.pointer, Stored::Bytes(full))?;

    // Read on, so its digest is checked against the pointer: no longer a size refusal.
    assert_matches!(outcome, Ok(Replayed::Mismatch(Mismatch::Digest { .. })));
    Ok(())
}

#[test]
fn a_re_run_pointer_is_refused_before_any_fetch() -> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;
    let attempt = NonZeroU64::new(2).ok_or("not zero")?;
    let pointer = EvidencePointer {
        run_attempt: attempt,
        ..persisted.entry.pointer.clone()
    };

    let (outcome, source) = replay(&persisted, &pointer, Stored::Bytes(persisted.bytes.clone()))?;

    assert_matches!(outcome, Err(ReplayError::NotFirstAttempt { attempt: a }) if a == attempt);
    assert_eq!(source.asked.borrow().len(), 0, "nothing was fetched");
    Ok(())
}

#[test]
fn a_truncated_bundle_is_an_error_not_a_pass() -> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;
    let half = persisted
        .bytes
        .get(..persisted.bytes.len() / 2)
        .ok_or("half the bundle")?
        .to_vec();

    let outcome = replay_repointed(&persisted, half)?;

    assert_matches!(
        outcome,
        Err(ReplayError::Unreadable(BundleShape::Truncated { .. }))
    );
    Ok(())
}

#[test]
fn a_bundle_that_is_not_json_is_an_error() -> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;

    let outcome = replay_repointed(&persisted, b"run-inputs?".to_vec())?;

    assert_matches!(
        outcome,
        Err(ReplayError::Unreadable(BundleShape::Syntax { line: 1, .. }))
    );
    Ok(())
}

#[test]
fn a_bundle_of_another_version_is_an_error() -> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;
    let bytes = tampered(&persisted, |bundle| {
        *at(bundle, "/schema")? = json!("assay.run-inputs/2");
        Ok(())
    })?;

    let outcome = replay_repointed(&persisted, bytes)?;

    assert_matches!(outcome, Err(ReplayError::Unreadable(BundleShape::Shape)));
    Ok(())
}

#[test]
fn a_bundle_repeating_a_key_is_an_error() -> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;
    let rest = persisted.bytes.strip_prefix(b"{").ok_or("an object")?;
    let bytes = [b"{\"events\":[],".as_slice(), rest].concat();

    let outcome = replay_repointed(&persisted, bytes)?;

    assert_matches!(outcome, Err(ReplayError::Unreadable(BundleShape::Shape)));
    Ok(())
}

#[test]
fn a_bundle_decided_under_another_rubric_version_is_an_error() -> Result<(), Box<dyn Error>> {
    let persisted = Persisted::new()?;
    let rubric: Rubric = toml::from_str(
        "version = \"2\"\nmajors_block_threshold = 3\nmax_requeues = 3\nmicro_roles = []\n",
    )?;

    let (outcome, _) = replay_under(
        &persisted,
        &persisted.entry.pointer,
        Stored::Bytes(persisted.bytes.clone()),
        &rubric,
    );

    assert_matches!(outcome, Err(ReplayError::RubricVersion));
    Ok(())
}
