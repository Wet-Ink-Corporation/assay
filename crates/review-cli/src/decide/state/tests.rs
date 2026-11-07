use std::assert_matches;

use super::*;

const DIGEST: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";

fn refusal() -> RefusalReason {
    RefusalReason::try_from(Reason {
        code: ReasonCode::GateError,
        detail: "artifact-digest: gates-static/temper.json".to_owned(),
        fp: None,
    })
    .expect("a refusal's reason")
}

fn states() -> [DecideState; 3] {
    [
        DecideState::Silent,
        DecideState::Refused(refusal()),
        DecideState::Decided {
            digest: DIGEST.parse().expect("a digest"),
        },
    ]
}

#[test]
fn each_state_reads_back_as_written() {
    for state in states() {
        let read = DecideState::read(&state.json());

        assert_eq!(read.as_ref().ok(), Some(&state), "{state:?}");
    }
}

#[test]
fn the_state_is_one_line_of_compact_json() {
    assert_eq!(
        String::from_utf8(
            DecideState::Decided {
                digest: DIGEST.parse().expect("a digest")
            }
            .json()
        ),
        Ok(format!(
            "{{\"bundle_digest\":\"{DIGEST}\",\"verdict\":\"decided\"}}"
        ))
    );
    assert_eq!(
        String::from_utf8(DecideState::Refused(refusal()).json()),
        Ok(
            "{\"reason\":{\"code\":\"gate-error\",\"detail\":\"artifact-digest: \
            gates-static/temper.json\"},\"verdict\":\"refused\"}"
                .to_owned()
        )
    );
}

#[test]
fn anything_but_exactly_a_state_is_refused() {
    for text in [
        "",
        "[]",
        "{}",
        r#"{"verdict":"silent","extra":1}"#,
        r#"{"verdict":"silent","verdict":"silent"}"#,
        r#"{"verdict":"published"}"#,
        r#"{"verdict":"decided"}"#,
        r#"{"verdict":"decided","bundle_digest":"sha256:11"}"#,
        r#"{"verdict":"refused"}"#,
        r#"{"verdict":"refused","reason":{"code":"gate-error"}}"#,
        r#"{"verdict":"refused","reason":{"code":"gate-error","detail":"x","fp":"0123456789abcdef"}}"#,
        r#"{"verdict":"refused","reason":{"code":"nope","detail":"x"}}"#,
    ] {
        assert_matches!(
            DecideState::read(text.as_bytes()),
            Err(DecideStateError::Parse | DecideStateError::Shape),
            "{text}"
        );
    }
}

#[test]
fn a_reason_no_refusal_gives_is_refused() {
    let text = r#"{"verdict":"refused","reason":{"code":"persist-error","detail":"upload"}}"#;

    assert_matches!(
        DecideState::read(text.as_bytes()),
        Err(DecideStateError::Reason(_))
    );
}

#[test]
fn a_state_over_its_cap_is_refused_unparsed() {
    let text = format!("{{\"verdict\":\"silent\"{}}}", " ".repeat(MAX_STATE_BYTES));

    assert_matches!(
        DecideState::read(text.as_bytes()),
        Err(DecideStateError::TooLarge)
    );
}
