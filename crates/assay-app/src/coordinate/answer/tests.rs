//! The strict reading of the coordinator's answer, one test per refusal.

use std::num::NonZeroU64;

use assay_domain::{Class, FindingKind, Severity, Side};
use proptest::prelude::*;
use serde_json::{Value, json};

use super::{COORDINATION_SCHEMA, CoordinationRefusal, Member, Plan, Shown, read};
use crate::coordinate::fixture::{of_class, valid};
use crate::coordinate::outcome::Merged;
use crate::validation::ValidFinding;

/// The position of the finding shown as `id` among the findings given: ids are offset by ten,
/// so a plan that kept ids instead of positions shows.
const fn index(id: usize) -> usize {
    id + 10
}

/// Three `logic` findings of `src/lib.rs` line 3: a major, a minor and a nit.
fn findings() -> Vec<ValidFinding> {
    [Severity::Major, Severity::Minor, Severity::Nit]
        .into_iter()
        .zip(1..)
        .map(|(severity, fp)| valid(fp, severity).expect("a 16-digit fingerprint"))
        .collect()
}

/// `findings` with `id`s of their position.
fn shown(findings: &[ValidFinding]) -> Vec<Shown<'_>> {
    findings
        .iter()
        .enumerate()
        .map(|(id, found)| Shown {
            index: index(id),
            found,
        })
        .collect()
}

fn refusal(answer: Value, findings: &[ValidFinding]) -> Option<CoordinationRefusal> {
    read(answer, &shown(findings)).err()
}

/// One group of `members`, without duplicates.
fn group(members: &[u64]) -> Value {
    json!({"findings": members, "duplicates": []})
}

/// The answer with finding `id` as a duplicate of finding `of`, in one group with every other
/// of the first `count` findings.
fn duplicate_of(id: u64, of: u64, count: u64) -> Value {
    let members: Vec<u64> = (0..count).filter(|&member| member != id).collect();
    json!({
        "groups": [{"findings": members, "duplicates": [{"id": id, "of": of}]}],
        "noise": []
    })
}

/// [`findings`] plus `extra` as id 3.
fn with(extra: ValidFinding) -> Vec<ValidFinding> {
    let mut findings = findings();
    findings.push(extra);
    findings
}

/// A minor of fingerprint 4, otherwise like [`findings`].
fn minor() -> ValidFinding {
    valid(4, Severity::Minor).expect("a 16-digit fingerprint")
}

fn line(line: u64) -> NonZeroU64 {
    NonZeroU64::new(line).expect("a line number")
}

#[test]
fn a_valid_answer_reads_into_positions_of_groups_duplicates_and_noise() {
    let findings = findings();
    let answer = json!({
        "groups": [{"findings": [0], "duplicates": [{"id": 1, "of": 0}]}, group(&[2])],
        "noise": [2]
    });

    let plan = read(answer, &shown(&findings));

    let member = |id, noise| Member {
        index: index(id),
        noise,
    };
    assert_eq!(
        plan,
        Ok(Plan {
            groups: vec![vec![member(0, false)], vec![member(2, true)]],
            duplicates: vec![Merged {
                index: index(1),
                into: index(0)
            }],
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
        .map(|answer| refusal(answer, &findings()))
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

    let refusals = [at_last, past_last, past_last_duplicate].map(|a| refusal(a, &findings()));

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
fn a_duplicate_of_an_unknown_finding_is_refused() {
    let answer = json!({
        "groups": [{"findings": [0, 2], "duplicates": [{"id": 1, "of": 3}]}],
        "noise": []
    });

    assert_eq!(
        refusal(answer, &findings()),
        Some(CoordinationRefusal::UnknownFinding)
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
        .map(|answer| refusal(answer, &findings()))
        .collect();

    assert_eq!(refusals, [Some(CoordinationRefusal::Repeated); 3]);
}

#[test]
fn a_finding_in_no_group_is_refused() {
    let answer = json!({"groups": [group(&[0, 2])], "noise": []});

    assert_eq!(
        refusal(answer, &findings()),
        Some(CoordinationRefusal::Missing)
    );
}

#[test]
fn a_group_without_findings_is_refused() {
    let answer = json!({"groups": [group(&[0, 1, 2]), group(&[])], "noise": []});

    assert_eq!(
        refusal(answer, &findings()),
        Some(CoordinationRefusal::EmptyGroup)
    );
}

#[test]
fn a_group_of_two_classes_is_refused() {
    let findings = with(of_class(4, Severity::Minor, Class::Security).expect("fingerprint"));
    let answer = json!({"groups": [group(&[0, 1, 2, 3])], "noise": []});

    assert_eq!(
        refusal(answer, &findings),
        Some(CoordinationRefusal::MixedClasses)
    );
}

#[test]
fn a_group_of_one_major_is_kept_and_of_two_is_refused() {
    let one_major = json!({"groups": [group(&[0, 1, 2]), group(&[3])], "noise": []});
    let two_majors = json!({"groups": [group(&[0, 3]), group(&[1, 2])], "noise": []});
    let findings = with(valid(4, Severity::Blocker).expect("fingerprint"));

    let refusals = [one_major, two_majors].map(|answer| refusal(answer, &findings));

    assert_eq!(refusals, [None, Some(CoordinationRefusal::SeveralMajors)]);
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

    let refusals = [elsewhere, itself, of_itself_beside_a_member].map(|a| refusal(a, &findings()));

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
    let mut other_file = minor();
    other_file.file_at_head = "src/other.rs".to_owned();
    let mut other_side = minor();
    other_side.finding.side = Some(Side::Left);

    let refusals =
        [other_file, other_side].map(|extra| refusal(duplicate_of(3, 0, 4), &with(extra)));

    assert_eq!(refusals, [Some(CoordinationRefusal::DuplicateElsewhere); 2]);
}

#[test]
fn a_major_or_blocker_duplicate_is_refused_even_under_a_blocker() {
    let mut findings = with(valid(4, Severity::Blocker).expect("fingerprint"));
    findings.push(valid(5, Severity::Blocker).expect("fingerprint"));
    // The major 0 under the blocker 3, and the blocker 4 under the blocker 3.
    let major_under_blocker = json!({
        "groups": [group(&[1, 2]), {"findings": [3], "duplicates": [{"id": 0, "of": 3}]},
                   group(&[4])],
        "noise": []
    });
    let blocker_under_blocker = json!({
        "groups": [group(&[0, 1, 2]), {"findings": [3], "duplicates": [{"id": 4, "of": 3}]}],
        "noise": []
    });

    let refusals =
        [major_under_blocker, blocker_under_blocker].map(|answer| refusal(answer, &findings));

    assert_eq!(refusals, [Some(CoordinationRefusal::DuplicateTooSevere); 2]);
}

#[test]
fn a_duplicate_of_another_class_is_refused() {
    let findings = with(of_class(4, Severity::Minor, Class::Security).expect("fingerprint"));

    assert_eq!(
        refusal(duplicate_of(3, 0, 4), &findings),
        Some(CoordinationRefusal::DuplicateOtherClass)
    );
}

#[test]
fn a_duplicate_of_another_kind_is_refused() {
    let mut omission = minor();
    omission.finding.kind = FindingKind::Omission;

    assert_eq!(
        refusal(duplicate_of(3, 0, 4), &with(omission)),
        Some(CoordinationRefusal::DuplicateOtherKind)
    );
}

#[test]
fn a_duplicate_reaching_the_keepers_line_is_kept_and_one_line_short_is_refused() {
    // The keeper 0 cites line 3.
    let mut reaching = minor();
    (reaching.finding.line, reaching.finding.end_line) = (line(1), Some(line(3)));
    let mut short = minor();
    (short.finding.line, short.finding.end_line) = (line(1), Some(line(2)));
    let mut far = minor();
    far.finding.line = line(900);

    let refusals = [reaching, short, far].map(|extra| refusal(duplicate_of(3, 0, 4), &with(extra)));

    assert_eq!(
        refusals,
        [
            None,
            Some(CoordinationRefusal::DuplicateApart),
            Some(CoordinationRefusal::DuplicateApart)
        ]
    );
}

#[test]
fn a_duplicate_that_would_hide_a_worse_or_new_finding_is_refused() {
    // The minor 1 under the nit 2.
    let more_severe = json!({
        "groups": [{"findings": [0, 2], "duplicates": [{"id": 1, "of": 2}]}],
        "noise": []
    });
    let mut pre_existing_keeper = findings();
    if let Some(found) = pre_existing_keeper.get_mut(0) {
        found.finding.pre_existing = true;
    }

    let refusals = [
        refusal(more_severe, &findings()),
        refusal(duplicate_of(1, 0, 3), &pre_existing_keeper),
    ];

    assert_eq!(refusals, [Some(CoordinationRefusal::DuplicateOutranks); 2]);
}

#[test]
fn noise_on_a_major_or_blocker_is_refused_and_on_a_minor_is_kept() {
    let findings = with(valid(4, Severity::Blocker).expect("fingerprint"));
    let on = |id: u64| json!({"groups": [group(&[0, 1, 2]), group(&[3])], "noise": [id]});

    let refusals = [on(1), on(0), on(3)].map(|answer| refusal(answer, &findings));

    assert_eq!(
        refusals,
        [
            None,
            Some(CoordinationRefusal::NoiseTooSevere),
            Some(CoordinationRefusal::NoiseTooSevere)
        ]
    );
}

#[test]
fn noise_on_a_duplicate_is_refused() {
    let answer = json!({
        "groups": [{"findings": [0, 1], "duplicates": [{"id": 2, "of": 1}]}],
        "noise": [2]
    });

    assert_eq!(
        refusal(answer, &findings()),
        Some(CoordinationRefusal::NoiseMerged)
    );
}

#[test]
fn noise_naming_an_unknown_id_is_refused() {
    let answer = json!({"groups": [group(&[0, 1, 2])], "noise": [3]});

    assert_eq!(
        refusal(answer, &findings()),
        Some(CoordinationRefusal::UnknownFinding)
    );
}

/// Six findings: one major, then minors and nits, all `logic` defects on line 3.
fn six() -> Vec<ValidFinding> {
    [
        Severity::Major,
        Severity::Minor,
        Severity::Nit,
        Severity::Minor,
        Severity::Nit,
        Severity::Minor,
    ]
    .into_iter()
    .zip(1..)
    .map(|(severity, fp)| valid(fp, severity).expect("a 16-digit fingerprint"))
    .collect()
}

/// Answers over [`six`]: a permutation cut into groups, some later members made duplicates of
/// their group's first, and up to two noise ids (one may be unknown). Many break a rule; many
/// do not.
fn answers() -> impl Strategy<Value = Value> {
    (
        Just((0_u64..6).collect::<Vec<_>>()).prop_shuffle(),
        prop::collection::vec(any::<bool>(), 6),
        prop::collection::vec(any::<bool>(), 6),
        prop::collection::vec(0_u64..7, 0..3),
    )
        .prop_map(|(order, cuts, duplicate, noise)| {
            let mut groups: Vec<(Vec<u64>, Vec<Value>)> = Vec::new();
            for ((id, cut), duplicate) in order.into_iter().zip(cuts).zip(duplicate) {
                match groups.last_mut() {
                    Some((members, duplicates)) if !cut => match members.first() {
                        Some(&of) if duplicate => duplicates.push(json!({"id": id, "of": of})),
                        _ => members.push(id),
                    },
                    _ => groups.push((vec![id], Vec::new())),
                }
            }
            let groups: Vec<Value> = groups
                .into_iter()
                .map(|(members, duplicates)| json!({"findings": members, "duplicates": duplicates}))
                .collect();
            json!({"groups": groups, "noise": noise})
        })
}

proptest! {
    #[test]
    fn every_answer_read_accepts_satisfies_the_schema(answer in answers()) {
        let schema: Value = serde_json::from_str(COORDINATION_SCHEMA).expect("the schema is JSON");
        let validator = jsonschema::draft202012::options()
            .build(&schema)
            .expect("the schema compiles");
        let findings = six();

        // A copy: `read` takes the answer by value, and the schema checks the same answer.
        let accepted = read(answer.clone(), &shown(&findings)).is_ok();

        prop_assert!(!accepted || validator.is_valid(&answer), "accepted but invalid: {answer}");
    }
}
