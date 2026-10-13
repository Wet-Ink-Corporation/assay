//! The strict reading of the coordinator's answer, one test per refusal.

use assay_domain::{Class, Severity};
use serde_json::{Value, json};

use super::{CoordinationRefusal, Plan, read};
use crate::coordinate::fixture::{of_class, valid};
use crate::validation::ValidFinding;

/// Three `logic` findings of `src/lib.rs`: a major, a minor and a nit.
fn shown() -> Vec<ValidFinding> {
    [Severity::Major, Severity::Minor, Severity::Nit]
        .into_iter()
        .zip(1..)
        .map(|(severity, fp)| valid(fp, severity).expect("a 16-digit fingerprint"))
        .collect()
}

fn refusal(answer: Value, findings: &[ValidFinding]) -> Option<CoordinationRefusal> {
    let shown: Vec<&ValidFinding> = findings.iter().collect();
    read(answer, &shown).err()
}

/// One group of `members`, without duplicates.
fn group(members: &[u64]) -> Value {
    json!({"findings": members, "duplicates": []})
}

#[test]
fn a_valid_answer_reads_into_groups_duplicates_and_noise() {
    let findings = shown();
    let shown: Vec<&ValidFinding> = findings.iter().collect();
    let answer = json!({
        "groups": [{"findings": [0], "duplicates": [{"id": 1, "of": 0}]}, group(&[2])],
        "noise": [2]
    });

    let plan = read(answer, &shown);

    assert_eq!(
        plan,
        Ok(Plan {
            groups: vec![vec![0], vec![2]],
            duplicates: vec![(1, 0)],
            noise: vec![false, false, true],
        })
    );
}

#[test]
fn anything_outside_the_exact_shape_is_a_shape_refusal() {
    let answers = [
        json!([group(&[0, 1, 2])]),
        json!({"groups": [group(&[0, 1, 2])]}),
        json!({"groups": [group(&[0, 1, 2])], "noise": [], "label": "x"}),
        json!({"groups": {}, "noise": []}),
        json!({"groups": [{"findings": [0, 1, 2]}], "noise": []}),
        json!({"groups": [{"findings": [0, 1], "duplicates": [{"id": 2}]}], "noise": []}),
        json!({"groups": [{"findings": [0, 1], "duplicates": [[2, 0]]}], "noise": []}),
        json!({"groups": [group(&[0, 1, 2])], "noise": [-1]}),
        json!({"groups": [{"findings": [0, 1, "2"], "duplicates": []}], "noise": []}),
        json!({"groups": [{"findings": [0, 1, 2.0], "duplicates": []}], "noise": []}),
    ];

    let refusals: Vec<_> = answers
        .into_iter()
        .map(|answer| refusal(answer, &shown()))
        .collect();

    assert_eq!(refusals, [Some(CoordinationRefusal::Shape); 10]);
}

#[test]
fn the_last_id_is_known_and_one_past_it_is_refused() {
    let at_last = json!({"groups": [group(&[0, 1, 2])], "noise": []});
    let past_last = json!({"groups": [group(&[0, 1, 2, 3])], "noise": []});
    let past_last_duplicate = json!({
        "groups": [{"findings": [0, 1, 2], "duplicates": [{"id": 3, "of": 0}]}],
        "noise": []
    });

    let refusals = [at_last, past_last, past_last_duplicate].map(|a| refusal(a, &shown()));

    assert_eq!(
        refusals,
        [
            None,
            Some(CoordinationRefusal::UnknownFinding),
            Some(CoordinationRefusal::UnknownFinding)
        ]
    );
}

#[test]
fn an_id_placed_twice_or_noise_listed_twice_is_refused() {
    let answers = [
        json!({"groups": [group(&[0, 1, 2]), group(&[0])], "noise": []}),
        json!({"groups": [{"findings": [0, 1, 2], "duplicates": [{"id": 1, "of": 0}]}],
               "noise": []}),
        json!({"groups": [group(&[0, 1, 2])], "noise": [2, 2]}),
    ];

    let refusals: Vec<_> = answers
        .into_iter()
        .map(|answer| refusal(answer, &shown()))
        .collect();

    assert_eq!(refusals, [Some(CoordinationRefusal::Repeated); 3]);
}

#[test]
fn a_finding_in_no_group_is_refused() {
    let answer = json!({"groups": [group(&[0, 2])], "noise": []});

    assert_eq!(
        refusal(answer, &shown()),
        Some(CoordinationRefusal::Missing)
    );
}

#[test]
fn a_group_without_findings_is_refused() {
    let answer = json!({"groups": [group(&[0, 1, 2]), group(&[])], "noise": []});

    assert_eq!(
        refusal(answer, &shown()),
        Some(CoordinationRefusal::EmptyGroup)
    );
}

#[test]
fn a_group_of_two_classes_is_refused() {
    let mut findings = shown();
    findings.push(of_class(4, Severity::Major, Class::Security).expect("fingerprint"));
    let answer = json!({"groups": [group(&[0, 1, 2, 3])], "noise": []});

    assert_eq!(
        refusal(answer, &findings),
        Some(CoordinationRefusal::MixedClasses)
    );
}

#[test]
fn a_duplicate_of_a_finding_outside_its_group_or_of_itself_is_refused() {
    let elsewhere = json!({
        "groups": [group(&[0]), {"findings": [2], "duplicates": [{"id": 1, "of": 0}]}],
        "noise": []
    });
    let itself = json!({
        "groups": [group(&[0, 2]), {"findings": [], "duplicates": [{"id": 1, "of": 1}]}],
        "noise": []
    });
    let of_itself_beside_a_member = json!({
        "groups": [{"findings": [0, 2], "duplicates": [{"id": 1, "of": 1}]}],
        "noise": []
    });

    let refusals = [elsewhere, itself, of_itself_beside_a_member].map(|a| refusal(a, &shown()));

    assert_eq!(
        refusals,
        [
            Some(CoordinationRefusal::DuplicateOutsideGroup),
            Some(CoordinationRefusal::EmptyGroup),
            Some(CoordinationRefusal::DuplicateOutsideGroup)
        ]
    );
}

#[test]
fn a_duplicate_in_another_file_or_side_is_refused() {
    let mut other_file = shown();
    if let Some(found) = other_file.get_mut(1) {
        found.file_at_head = "src/other.rs".to_owned();
    }
    let mut other_side = shown();
    if let Some(found) = other_side.get_mut(1) {
        found.finding.side = Some(assay_domain::Side::Left);
    }
    let answer = json!({
        "groups": [{"findings": [0, 2], "duplicates": [{"id": 1, "of": 0}]}],
        "noise": []
    });

    // A copy per case: `read` takes the answer by value, as the coordinator hands it over.
    let refusals = [other_file, other_side].map(|found| refusal(answer.clone(), &found));

    assert_eq!(refusals, [Some(CoordinationRefusal::DuplicateElsewhere); 2]);
}

#[test]
fn a_duplicate_that_would_hide_a_worse_or_new_finding_is_refused() {
    let more_severe = json!({
        "groups": [{"findings": [1, 2], "duplicates": [{"id": 0, "of": 1}]}],
        "noise": []
    });
    let mut pre_existing_keeper = shown();
    if let Some(found) = pre_existing_keeper.get_mut(0) {
        found.finding.pre_existing = true;
    }
    let new_under_pre_existing = json!({
        "groups": [{"findings": [0, 2], "duplicates": [{"id": 1, "of": 0}]}],
        "noise": []
    });

    let refusals = [
        refusal(more_severe, &shown()),
        refusal(new_under_pre_existing, &pre_existing_keeper),
    ];

    assert_eq!(refusals, [Some(CoordinationRefusal::DuplicateOutranks); 2]);
}

#[test]
fn noise_on_a_major_is_refused_and_on_a_minor_is_kept() {
    let on_minor = json!({"groups": [group(&[0, 1, 2])], "noise": [1]});
    let on_major = json!({"groups": [group(&[0, 1, 2])], "noise": [0]});

    let refusals = [on_minor, on_major].map(|answer| refusal(answer, &shown()));

    assert_eq!(refusals, [None, Some(CoordinationRefusal::NoiseTooSevere)]);
}

#[test]
fn noise_on_a_duplicate_is_refused() {
    let answer = json!({
        "groups": [{"findings": [0, 1], "duplicates": [{"id": 2, "of": 1}]}],
        "noise": [2]
    });

    assert_eq!(
        refusal(answer, &shown()),
        Some(CoordinationRefusal::NoiseMerged)
    );
}

#[test]
fn noise_naming_an_unknown_id_is_refused() {
    let answer = json!({"groups": [group(&[0, 1, 2])], "noise": [3]});

    assert_eq!(
        refusal(answer, &shown()),
        Some(CoordinationRefusal::UnknownFinding)
    );
}

#[test]
fn the_schema_requires_both_keys_and_no_others() {
    let schema: Value = serde_json::from_str(super::COORDINATION_SCHEMA).unwrap_or(Value::Null);

    assert_eq!(
        (&schema["required"], &schema["additionalProperties"]),
        (&json!(["groups", "noise"]), &json!(false))
    );
}
