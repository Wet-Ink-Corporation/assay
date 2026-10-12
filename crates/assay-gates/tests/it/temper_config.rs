//! Step 3, the gate-config diff of `.claude/temper.json`, and the vendored copy's drift from the
//! pin (`docs/architecture/03-contracts.md` §2.1, `05-security.md` T4).

use std::assert_matches;
use std::error::Error;

use assay_app::{Conclusion, GateJob, GateOutcome};
use assay_domain::Severity;
use assay_gates::{Revision, TemperError};

use crate::temper_support::{CONFIG, EXCEPTIONS, PINNED, Pr, TEMPER_JSON};

const FAILED: GateOutcome = GateOutcome::Concluded(Conclusion::Fail);
const PASSED: GateOutcome = GateOutcome::Concluded(Conclusion::Pass);

/// [`TEMPER_JSON`] with the gate's step timeout raised.
fn longer_steps() -> String {
    TEMPER_JSON.replace("\"stepTimeoutSec\": 900", "\"stepTimeoutSec\": 1800")
}

/// The 1-based line of `text` that holds `"key":`.
fn key_line(text: &str, key: &str) -> Result<u64, Box<dyn Error>> {
    let quoted = format!("\"{key}\":");
    let index = text
        .lines()
        .position(|line| line.contains(&quoted))
        .ok_or_else(|| format!("{key} is not in the text"))?;
    Ok(u64::try_from(index)? + 1)
}

#[test]
fn a_changed_gate_key_fails_at_its_line() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    let head = longer_steps();
    pr.set(Revision::Head, CONFIG, &head);

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (
            FAILED,
            vec![(
                "gate-config".to_owned(),
                CONFIG.to_owned(),
                key_line(&head, "gate")?,
                Severity::Blocker
            )]
        ),
        "the gate reads `gate`"
    );
    Ok(())
}

#[test]
fn every_compared_key_is_checked() -> Result<(), Box<dyn Error>> {
    for (from, to) in [
        ("\"exempt\": []", "\"exempt\": [\"src/**\"]"),
        ("\"ownLints\": []", "\"ownLints\": [\"x\"]"),
        ("\"review\": \"required\"", "\"review\": \"off\""),
        ("\"skillGate\": true", "\"skillGate\": false"),
        (
            "\"askOnNewDependency\": true",
            "\"askOnNewDependency\": false",
        ),
        (
            "[\"serde\", \"thiserror\"]",
            "[\"serde\", \"thiserror\", \"tokio\"]",
        ),
        (
            "\"skillGate\": true,",
            "\"skillGate\": true, \"protected\": [],",
        ),
    ] {
        let mut pr = Pr::new()?;
        pr.set(Revision::Head, CONFIG, &TEMPER_JSON.replace(from, to));

        let outcome = pr.outcome()?;

        assert_eq!(outcome.0, FAILED, "{from} -> {to}");
    }
    Ok(())
}

#[test]
fn a_removed_key_fails() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(
        Revision::Head,
        CONFIG,
        &TEMPER_JSON.replace("  \"skillGate\": true,\n", ""),
    );

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (
            FAILED,
            vec![(
                "gate-config".to_owned(),
                CONFIG.to_owned(),
                key_line(TEMPER_JSON, "skillGate")?,
                Severity::Blocker
            )]
        ),
        "a removed key is found at its base line"
    );
    Ok(())
}

#[test]
fn a_key_temper_does_not_steer_with_may_change() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(
        Revision::Head,
        CONFIG,
        &TEMPER_JSON.replacen("{\n", "{\n  \"telemetry\": false,\n", 1),
    );

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (PASSED, Vec::new()),
        "only the listed keys are compared"
    );
    Ok(())
}

/// An exceptions file admitting `gate` changing from `base` to `head` (JSON texts).
fn admits_gate(base: &str, head: &str) -> String {
    format!(
        "[[gate_config]]\nkey = \"gate\"\nbase = '{base}'\nhead = '{head}'\n\
         reason = \"the test job outgrew 15 minutes\"\n"
    )
}

const GATE_900: &str = r#"{"allFeatures": true, "doc": true, "scan": true, "stepTimeoutSec": 900}"#;
const GATE_1200: &str =
    r#"{"allFeatures": true, "doc": true, "scan": true, "stepTimeoutSec": 1200}"#;
const GATE_1800: &str =
    r#"{"allFeatures": true, "doc": true, "scan": true, "stepTimeoutSec": 1800}"#;

#[test]
fn a_change_allowlisted_with_its_exact_base_and_head_values_passes() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(Revision::Head, CONFIG, &longer_steps());
    pr.set(
        Revision::Base,
        EXCEPTIONS,
        &admits_gate(GATE_900, GATE_1800),
    );

    let outcome = pr.outcome()?;

    assert_eq!(outcome, (PASSED, Vec::new()), "the base admits this change");
    Ok(())
}

#[test]
fn an_allowlist_for_another_head_value_does_not_admit_the_change() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(Revision::Head, CONFIG, &longer_steps());
    pr.set(
        Revision::Base,
        EXCEPTIONS,
        &admits_gate(GATE_900, GATE_1200),
    );

    let outcome = pr.outcome()?;

    assert_eq!(outcome.0, FAILED, "the allowlist binds the head value");
    Ok(())
}

#[test]
fn an_allowlist_for_another_base_value_does_not_admit_the_change() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(Revision::Head, CONFIG, &longer_steps());
    pr.set(
        Revision::Base,
        EXCEPTIONS,
        &admits_gate(GATE_1200, GATE_1800),
    );

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome.0, FAILED,
        "the allowlist binds the value it changes from, so it cannot be replayed"
    );
    Ok(())
}

#[test]
fn an_allowlist_entry_for_an_uncompared_key_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(
        Revision::Base,
        EXCEPTIONS,
        "[[gate_config]]\nkey = \"gates\"\nbase = '{}'\nhead = '{}'\nreason = \"typo\"\n",
    );

    let result = pr.run(GateJob::Static)?;

    assert_matches!(
        result,
        Err(TemperError::Malformed { revision: Revision::Base, path, .. }) if path == EXCEPTIONS
    );
    Ok(())
}

#[test]
fn a_temper_json_missing_or_unparsable_on_either_side_is_a_gate_error() -> Result<(), Box<dyn Error>>
{
    for side in [Revision::Base, Revision::Head] {
        let mut pr = Pr::new()?;
        pr.remove(side, CONFIG);
        assert_matches!(
            pr.run(GateJob::Static)?,
            Err(TemperError::Missing { revision, path }) if revision == side && path == CONFIG
        );
        for text in ["{", "[]"] {
            let mut pr = Pr::new()?;
            pr.set(side, CONFIG, text);
            assert_matches!(
                pr.run(GateJob::Static)?,
                Err(TemperError::Malformed { revision, path, .. }) if revision == side && path == CONFIG,
                "{text}"
            );
        }
    }
    Ok(())
}

#[test]
fn a_vendored_copy_that_drifted_from_the_pin_is_a_major_finding() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    for (path, text) in PINNED {
        pr.set(Revision::Head, &format!(".claude/temper/{path}"), text);
    }
    pr.set(
        Revision::Head,
        ".claude/temper/lib/scan.mjs",
        "export function scanSource() { return; }\n",
    );

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (
            PASSED,
            vec![(
                "vendored-drift".to_owned(),
                ".claude/temper/lib/scan.mjs".to_owned(),
                1,
                Severity::Major
            )]
        ),
        "the copy is ignored, so its drift is reported but does not decide the gate"
    );
    Ok(())
}

#[test]
fn a_missing_or_extra_vendored_file_is_drift() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    for (path, text) in PINNED {
        pr.set(Revision::Head, &format!(".claude/temper/{path}"), text);
    }
    pr.remove(Revision::Head, ".claude/temper/VERSION");
    pr.link(Revision::Head, ".claude/temper/lib/zz.mjs", "scan.mjs");

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (
            PASSED,
            vec![(
                "vendored-drift".to_owned(),
                ".claude/temper/VERSION".to_owned(),
                1,
                Severity::Major
            )]
        ),
        "one finding names the first drifted path"
    );
    Ok(())
}

#[test]
fn a_vendored_copy_matching_the_pin_or_absent_is_not_drift() -> Result<(), Box<dyn Error>> {
    let mut absent = Pr::new()?;
    let mut matching = Pr::new()?;
    for (path, text) in PINNED {
        matching.set(Revision::Head, &format!(".claude/temper/{path}"), text);
    }

    let outcomes = (absent.outcome()?, matching.outcome()?);

    assert_eq!(
        outcomes,
        ((PASSED, Vec::new()), (PASSED, Vec::new())),
        "neither differs from the pin"
    );
    Ok(())
}

#[test]
fn an_unreadable_pinned_copy_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    std::fs::remove_dir_all(&pr.pinned)?;

    let result = pr.run(GateJob::Static)?;

    assert_matches!(result, Err(TemperError::Pinned { .. }));
    Ok(())
}
