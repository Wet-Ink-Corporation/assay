//! A command's canonical line (`Command::line`, D1 of #61): the text a command record holds, so
//! that every later read re-parses it to the command decide-only recorded, byte for byte. A
//! `RecordedCommand` is such a command with its line, parsed once (#353 F4).

use proptest::prelude::*;
use proptest::sample::select;
use review_app::{
    AckedSet, Command, MAX_COMMAND_CHARS, MAX_COMMAND_REASON_CHARS, ParseRecordedCommandError,
    Parsed, RecordedCommand, Sha256Digest, parse_command,
};
use review_domain::{Fingerprint, OverrideScope, ParseFingerprintError, RequiredRole};

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

    #[test]
    fn every_recorded_command_is_held_with_its_line(command in recorded_command()) {
        let line = command.line();

        let held = RecordedCommand::new(command.clone());
        let parsed = line.parse::<RecordedCommand>();

        let held = held.expect("only a recheck is refused");
        prop_assert_eq!(held.as_command(), &command);
        prop_assert_eq!(held.as_str(), line.as_str());
        prop_assert_eq!(parsed, Ok(held));
    }
}

#[test]
fn a_recheck_is_never_a_recorded_command() {
    assert_eq!(RecordedCommand::new(Command::Recheck), None);
}

/// A dismiss of the recorded finding with `reason`, built by hand (its fields are public).
fn dismiss(reason: &str) -> Result<Command, ParseFingerprintError> {
    Ok(Command::Dismiss {
        finding: "0123456789abcdef".parse()?,
        reason: reason.to_owned(),
    })
}

// The grammar refuses a control character in a reason, and every other token is a fixed word,
// a fingerprint or a digest, so a hand-built command holding one has no line that reads back
// as it, and is never a recorded command (formerly
// `ledger_streams::a_command_whose_reason_holds_a_control_character_is_never_written`).
#[test]
fn a_command_whose_reason_holds_a_control_character_is_never_recorded()
-> Result<(), ParseFingerprintError> {
    for reason in [
        "flaky\u{1b}[2J gate",
        "two\nlines",
        "carriage\r",
        "tab\there",
    ] {
        // Its line is malformed, or (a line break) reads as another command: the first line's.
        let command = dismiss(reason)?;
        assert_ne!(
            parse_command(&command.line()),
            // A copy to compare against; `new` takes the command itself.
            Parsed::Command(command.clone()),
            "{reason:?}"
        );

        assert_eq!(RecordedCommand::new(command), None, "{reason:?}");
    }
    Ok(())
}

// The grammar caps a reason at `MAX_COMMAND_REASON_CHARS`, which bounds every recorded line
// well under `MAX_COMMAND_CHARS` (the schema's `maxLength`), so it is the grammar's cap that
// refuses a long command (formerly
// `ledger_streams::a_command_whose_reason_passes_the_grammars_cap_is_never_written`).
#[test]
fn a_command_whose_reason_passes_the_grammars_cap_is_never_recorded()
-> Result<(), ParseFingerprintError> {
    let command = dismiss(&"y".repeat(MAX_COMMAND_REASON_CHARS + 1))?;
    assert!(
        command.line().chars().count() <= MAX_COMMAND_CHARS,
        "under the line cap"
    );

    assert_eq!(RecordedCommand::new(command), None);
    Ok(())
}

#[test]
fn a_hand_built_command_whose_line_reads_back_otherwise_is_never_recorded()
-> Result<(), ParseFingerprintError> {
    for reason in ["", " leading", "trailing "] {
        assert_eq!(RecordedCommand::new(dismiss(reason)?), None, "{reason:?}");
    }
    let at_cap = dismiss(&"y".repeat(MAX_COMMAND_REASON_CHARS))?;
    assert!(
        RecordedCommand::new(at_cap).is_some(),
        "the cap itself is recorded"
    );
    Ok(())
}

#[test]
fn text_that_is_no_recorded_commands_canonical_line_is_refused() {
    for (text, problem) in [
        ("/assay recheck", ParseRecordedCommandError::Recheck),
        ("/assay approve", ParseRecordedCommandError::Unparsed),
        ("/assay waive nope", ParseRecordedCommandError::Unparsed),
        (
            "waive 0123456789abcdef",
            ParseRecordedCommandError::Unparsed,
        ),
        (
            "/assay waive 0123456789abcdef\nmore",
            ParseRecordedCommandError::NotCanonical,
        ),
        (
            "/assay waive 0123456789abcdef\r",
            ParseRecordedCommandError::NotCanonical,
        ),
    ] {
        assert_eq!(text.parse::<RecordedCommand>(), Err(problem), "{text:?}");
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
