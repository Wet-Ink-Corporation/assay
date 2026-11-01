//! A command's canonical line (`Command::line`, D1 of #61): the text a command record holds, so
//! that every later read re-parses it to the command decide-only recorded, byte for byte.

use assay_app::{
    AckedSet, Command, MAX_COMMAND_CHARS, MAX_COMMAND_REASON_CHARS, Parsed, Sha256Digest,
    parse_command,
};
use assay_domain::{Fingerprint, OverrideScope, RequiredRole};
use proptest::prelude::*;
use proptest::sample::select;

/// A fingerprint: sixteen lowercase hex digits.
fn fingerprint() -> impl Strategy<Value = Fingerprint> {
    "[0-9a-f]{16}".prop_map(|text| text.parse().expect("sixteen hex digits are a fingerprint"))
}

/// A sha256 digest, `sha256:<64 hex>`.
fn digest() -> impl Strategy<Value = Sha256Digest> {
    "[0-9a-f]{64}".prop_map(|hex| {
        format!("sha256:{hex}")
            .parse()
            .expect("sixty-four hex digits are a digest")
    })
}

/// A reason the grammar takes: 1 to 200 characters, no control character, no space at either
/// end (Unicode letters and inner spaces included).
fn reason() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9.:,!?'é漢 -]{1,200}"
        .prop_filter("no space at either end", |reason| reason.trim() == reason)
}

/// Every command a record holds: all but `recheck`.
fn recorded_command() -> impl Strategy<Value = Command> {
    prop_oneof![
        fingerprint().prop_map(|finding| Command::Waive { finding }),
        (select(OverrideScope::ALL.to_vec()), digest(), reason()).prop_map(
            |(scope, reasons_digest, reason)| Command::Override {
                scope,
                reasons_digest,
                reason,
            }
        ),
        (fingerprint(), reason())
            .prop_map(|(finding, reason)| Command::Dismiss { finding, reason }),
        select(RequiredRole::ALL.to_vec()).prop_map(|role| Command::Retry { role }),
        digest().prop_map(|digest| Command::WeighInAck {
            set: AckedSet::Digest(digest)
        }),
        Just(Command::WeighInAck {
            set: AckedSet::None
        }),
    ]
}

proptest! {
    #[test]
    fn every_recorded_command_reads_back_from_its_line(command in recorded_command()) {
        let line = command.line();

        prop_assert_eq!(parse_command(&line), Parsed::Command(command));
        prop_assert!(line.chars().count() <= MAX_COMMAND_CHARS);
    }
}

#[test]
fn a_line_is_the_prefix_the_word_and_each_argument_after_one_space() {
    let digest: Sha256Digest = format!("sha256:{}", "ab".repeat(32))
        .parse()
        .expect("a digest");
    let command = Command::Override {
        scope: OverrideScope::Llm,
        reasons_digest: digest,
        reason: "the model is down".to_owned(),
    };

    assert_eq!(
        command.line(),
        format!(
            "/assay override llm sha256:{} the model is down",
            "ab".repeat(32)
        )
    );
    assert_eq!(Command::Recheck.line(), "/assay recheck");
}

#[test]
fn the_longest_override_line_fits_a_command_record() {
    let digest: Sha256Digest = format!("sha256:{}", "f".repeat(64))
        .parse()
        .expect("a digest");
    let command = Command::Override {
        scope: OverrideScope::All,
        reasons_digest: digest,
        reason: "漢".repeat(MAX_COMMAND_REASON_CHARS),
    };

    let line = command.line();

    assert_eq!(line.chars().count(), 20 + 71 + 1 + MAX_COMMAND_REASON_CHARS);
    assert!(line.chars().count() <= MAX_COMMAND_CHARS);
    assert_eq!(parse_command(&line), Parsed::Command(command));
}
