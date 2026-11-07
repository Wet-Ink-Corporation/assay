use std::assert_matches;

use proptest::prelude::*;

use super::*;
use crate::ci::Outputs;

const HEAD: &str = "1111111111111111111111111111111111111111";
const BASE: &str = "2222222222222222222222222222222222222222";
const BEFORE: &str = "3333333333333333333333333333333333333333";

fn id(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("a positive test id")
}

fn commit(text: &str) -> CommitId {
    text.parse().expect("a full commit id")
}

fn identity(trigger: Trigger) -> Identity {
    Identity {
        pr: id(42),
        head_sha: commit(HEAD),
        base_sha: commit(BASE),
        base_ref: "main".to_owned(),
        draft: false,
        author: UserId(id(7)),
        sender: UserId(id(8)),
        trigger,
        default_branch: "main".to_owned(),
    }
}

/// `toJSON(needs)` for a `guard` that ended with `result` and wrote `outputs`.
fn needs_text(result: &str, outputs: &[(&str, &str)]) -> String {
    let outputs: serde_json::Map<String, serde_json::Value> = outputs
        .iter()
        .map(|&(name, value)| (name.to_owned(), value.into()))
        .collect();
    serde_json::json!({ "guard": { "result": result, "outputs": outputs } }).to_string()
}

/// The needs a consumer of `guard` reads after `identity`'s outputs went through the
/// `GITHUB_OUTPUT` writer.
fn through_outputs(identity: &Identity) -> Needs {
    let mut file = Vec::new();
    let mut outputs = Outputs::new(&mut file);
    for (name, value) in identity.outputs() {
        outputs.write(name, &value).expect("single-line values");
    }
    let text = String::from_utf8(file).expect("UTF-8 outputs");
    let pairs: Vec<(&str, &str)> = text
        .lines()
        .map(|line| line.split_once('=').expect("name=value"))
        .collect();
    Needs::parse(needs_text("success", &pairs).as_bytes(), &[JobName::Guard])
        .expect("guard's needs")
}

fn decoded(outputs: &[(&str, &str)]) -> Result<Identity, NeedsError> {
    let needs = Needs::parse(needs_text("success", outputs).as_bytes(), &[JobName::Guard])?;
    Identity::from_needs(&needs)
}

const OPENED: [(&str, &str); 10] = [
    ("run", "true"),
    ("pr", "42"),
    ("head_sha", HEAD),
    ("base_sha", BASE),
    ("base_ref", "main"),
    ("draft", "false"),
    ("author_id", "7"),
    ("sender_id", "8"),
    ("trigger", "opened"),
    ("default_branch", "main"),
];

#[test]
fn encodes_the_documented_outputs() {
    let outputs = identity(Trigger::Opened).outputs();

    let expected: Vec<(OutputName, String)> = OPENED
        .iter()
        .map(|&(name, value)| {
            let name = OutputName::GUARD
                .into_iter()
                .find(|output| output.as_str() == name)
                .expect("a guard output");
            (name, value.to_owned())
        })
        .collect();
    assert_eq!(outputs, expected);
}

#[test]
fn decodes_the_documented_outputs() {
    let identity_read = decoded(&OPENED).expect("guard's documented outputs");

    assert_eq!(identity_read, identity(Trigger::Opened));
}

#[test]
fn round_trips_every_trigger() {
    for trigger in [
        Trigger::Opened,
        Trigger::Synchronize {
            before: commit(BEFORE),
        },
        Trigger::Reopened,
        Trigger::ReadyForReview,
        Trigger::Edited,
        Trigger::Dispatch,
    ] {
        let identity = identity(trigger);

        let read = Identity::from_needs(&through_outputs(&identity)).expect("its own outputs");

        assert_eq!(read, identity);
    }
}

#[test]
fn before_is_written_for_synchronize_only() {
    let push = identity(Trigger::Synchronize {
        before: commit(BEFORE),
    });

    let before = |identity: &Identity| {
        identity
            .outputs()
            .into_iter()
            .find(|&(name, _)| name == OutputName::Before)
            .map(|(_, value)| value)
    };

    assert_eq!(before(&push), Some(BEFORE.to_owned()));
    assert_eq!(before(&identity(Trigger::Opened)), None);
}

#[test]
fn refuses_a_guard_that_did_not_succeed() {
    for (word, result) in [
        ("failure", JobResult::Failure),
        ("cancelled", JobResult::Cancelled),
        ("skipped", JobResult::Skipped),
    ] {
        let needs = Needs::parse(needs_text(word, &OPENED).as_bytes(), &[JobName::Guard])
            .expect("guard's needs");

        assert_matches!(
            Identity::from_needs(&needs),
            Err(NeedsError::NotSucceeded { job: JobName::Guard, result: read }) if read == result,
            "{word}"
        );
    }
}

#[test]
fn refuses_needs_without_guard() {
    let needs = Needs::parse(b"{}", &[]).expect("empty needs");

    assert_matches!(
        Identity::from_needs(&needs),
        Err(NeedsError::MissingJob {
            job: JobName::Guard
        })
    );
}

#[test]
fn refuses_an_output_guard_does_not_write() {
    let mut outputs = OPENED.to_vec();
    outputs.push(("tier", "full"));

    assert_matches!(
        decoded(&outputs),
        Err(NeedsError::UnexpectedOutput { job: JobName::Guard, name }) if name == "tier"
    );
}

#[test]
fn refuses_each_missing_or_empty_required_output() {
    for (index, &(missing, _)) in OPENED.iter().enumerate() {
        let name = OutputName::GUARD
            .into_iter()
            .find(|output| output.as_str() == missing)
            .expect("a guard output");
        let absent: Vec<_> = OPENED
            .iter()
            .enumerate()
            .filter(|&(other, _)| other != index)
            .map(|(_, &pair)| pair)
            .collect();
        let mut empty = OPENED.to_vec();
        if let Some(pair) = empty.get_mut(index) {
            pair.1 = "";
        }

        for outputs in [absent, empty] {
            assert_matches!(
                decoded(&outputs),
                Err(NeedsError::MissingOutput { job: JobName::Guard, name: read }) if read == name,
                "{missing}"
            );
        }
    }
}

#[test]
fn refuses_each_invalid_value() {
    let cases = [
        ("run", "false"),
        ("run", "TRUE"),
        ("pr", "0"),
        ("pr", "042"),
        ("head_sha", "abc"),
        ("base_sha", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
        ("draft", "yes"),
        ("author_id", "-7"),
        ("sender_id", "8.0"),
        ("trigger", "labeled"),
        ("base_ref", "ma\u{7}in"),
        ("default_branch", "ma\u{1b}in"),
    ];
    for (output, value) in cases {
        let name = OutputName::GUARD
            .into_iter()
            .find(|name| name.as_str() == output)
            .expect("a guard output");
        let outputs: Vec<(&str, &str)> = OPENED
            .iter()
            .map(|&(other, original)| (other, if other == output { value } else { original }))
            .collect();

        assert_matches!(
            decoded(&outputs),
            Err(NeedsError::Output { job: JobName::Guard, name: read }) if read == name,
            "{output}={value}"
        );
    }
}

#[test]
fn refuses_before_without_synchronize_and_synchronize_without_before() {
    let mut stray = OPENED.to_vec();
    stray.push(("before", BEFORE));
    let missing: Vec<_> = OPENED
        .iter()
        .map(|&(name, value)| {
            (
                name,
                if name == "trigger" {
                    "synchronize"
                } else {
                    value
                },
            )
        })
        .collect();

    assert_matches!(
        decoded(&stray),
        Err(NeedsError::Output {
            job: JobName::Guard,
            name: OutputName::Before
        })
    );
    assert_matches!(
        decoded(&missing),
        Err(NeedsError::MissingOutput {
            job: JobName::Guard,
            name: OutputName::Before
        })
    );
}

/// Any valid identity: positive ids, full commit ids and branch names of printable ASCII.
fn identities() -> impl Strategy<Value = Identity> {
    let commit = || "[0-9a-f]{40}".prop_map(|text| commit(&text));
    let user = || (1..=u64::MAX).prop_map(|value| UserId(id(value)));
    let branch = || "[A-Za-z0-9._/-]{1,40}";
    let trigger = prop_oneof![
        Just(Trigger::Opened),
        commit().prop_map(|before| Trigger::Synchronize { before }),
        Just(Trigger::Reopened),
        Just(Trigger::ReadyForReview),
        Just(Trigger::Edited),
        Just(Trigger::Dispatch),
    ];
    (
        (1..=u64::MAX, commit(), commit(), branch(), any::<bool>()),
        (user(), user(), trigger, branch()),
    )
        .prop_map(
            |(
                (pr, head_sha, base_sha, base_ref, draft),
                (author, sender, trigger, default_branch),
            )| {
                Identity {
                    pr: id(pr),
                    head_sha,
                    base_sha,
                    base_ref,
                    draft,
                    author,
                    sender,
                    trigger,
                    default_branch,
                }
            },
        )
}

proptest! {
    #[test]
    fn decode_inverts_encode(identity in identities()) {
        let read = Identity::from_needs(&through_outputs(&identity));

        prop_assert_eq!(read.ok(), Some(identity));
    }
}
