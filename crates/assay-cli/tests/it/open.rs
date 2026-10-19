//! `assay open` (#54, S0): its run identity from the Actions environment, the re-run refusal
//! before any token is minted (R3-05), and the binary's answer until the live transport (#273).

use std::error::Error;
use std::ffi::OsString;
use std::num::NonZeroU64;
use std::process::Command;

use assay_cli::{OpenCommand, OpenUsage, OpenVariables};
use assay_domain::RunUid;

/// `n` (at least 1) as a non-zero id.
fn id(n: u64) -> NonZeroU64 {
    NonZeroU64::MIN.saturating_add(n.saturating_sub(1))
}

fn variables(run_id: &str, attempt: &str, repository_id: &str) -> OpenVariables {
    OpenVariables {
        run_id: Some(OsString::from(run_id)),
        run_attempt: Some(OsString::from(attempt)),
        repository_id: Some(OsString::from(repository_id)),
    }
}

#[test]
fn a_first_attempt_is_read_from_the_actions_environment() {
    let parsed = OpenCommand::parse(Vec::new(), variables("39168909542", "1", "42"));

    assert_eq!(
        parsed,
        Ok(OpenCommand {
            run_uid: RunUid {
                repo_id: id(42),
                run_id: id(39_168_909_542),
                run_attempt: id(1),
            }
        })
    );
}

#[test]
fn a_re_run_is_refused_before_anything_is_wired() {
    let parsed = OpenCommand::parse(Vec::new(), variables("39168909542", "2", "42"));

    assert_eq!(parsed, Err(OpenUsage::NotFirstAttempt { attempt: id(2) }));
}

#[test]
fn a_missing_or_malformed_variable_is_refused_by_name() {
    let missing = OpenVariables {
        run_attempt: None,
        ..variables("1", "1", "42")
    };

    assert_eq!(
        [
            OpenCommand::parse(Vec::new(), missing),
            OpenCommand::parse(Vec::new(), variables("0", "1", "42")),
            OpenCommand::parse(Vec::new(), variables("1", "1", "+42")),
            OpenCommand::parse(Vec::new(), variables("1", "", "42")),
        ],
        [
            Err(OpenUsage::Missing("GITHUB_RUN_ATTEMPT")),
            Err(OpenUsage::Invalid("GITHUB_RUN_ID")),
            Err(OpenUsage::Invalid("GITHUB_REPOSITORY_ID")),
            Err(OpenUsage::Invalid("GITHUB_RUN_ATTEMPT")),
        ]
    );
}

#[test]
fn open_takes_no_arguments() {
    let parsed = OpenCommand::parse(vec![OsString::from("--json")], variables("1", "1", "42"));

    assert_eq!(parsed, Err(OpenUsage::Arguments));
}

/// `assay open` run with the Actions variables of run attempt `attempt`.
fn run_binary(attempt: &str) -> Result<std::process::Output, std::io::Error> {
    Command::new(env!("CARGO_BIN_EXE_assay"))
        .arg("open")
        .env("GITHUB_RUN_ID", "39168909542")
        .env("GITHUB_RUN_ATTEMPT", attempt)
        .env("GITHUB_REPOSITORY_ID", "42")
        .output()
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn the_binary_refuses_a_re_run_and_names_the_missing_transport() -> Result<(), Box<dyn Error>> {
    let rerun = run_binary("2")?;
    let unwired = run_binary("1")?;

    assert_eq!(
        (rerun.status.code(), rerun.stdout.is_empty()),
        (Some(2), true)
    );
    let refusal = String::from_utf8(rerun.stderr)?;
    assert!(refusal.contains("attempt 2"), "{refusal}");
    assert_eq!(
        (unwired.status.code(), unwired.stdout.is_empty()),
        (Some(2), true)
    );
    let message = String::from_utf8(unwired.stderr)?;
    assert!(message.contains("(#273)"), "{message}");
    Ok(())
}
