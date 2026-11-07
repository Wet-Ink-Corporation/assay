//! `assay reconcile [--ledger]` (#73): only a first run attempt reaches a sweep (R3-05), which
//! then stops at the missing live transport (#273).

use std::error::Error;
use std::process::Command;

/// The exit code and stderr of `assay reconcile` with `args`, under `GITHUB_RUN_ATTEMPT` set to
/// `attempt` (unset for `None`).
fn reconcile(
    args: &[&str],
    attempt: Option<&str>,
) -> Result<(Option<i32>, String), Box<dyn Error>> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_assay"));
    command
        .arg("reconcile")
        .args(args)
        .env_remove("GITHUB_RUN_ATTEMPT");
    if let Some(attempt) = attempt {
        command.env("GITHUB_RUN_ATTEMPT", attempt);
    }
    let output = command.output()?;
    Ok((output.status.code(), String::from_utf8(output.stderr)?))
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_re_run_is_refused_before_anything_is_read() -> Result<(), Box<dyn Error>> {
    for args in [&[][..], &["--ledger"]] {
        let (code, stderr) = reconcile(args, Some("2"))?;

        assert_eq!(code, Some(2), "{args:?}");
        assert!(
            stderr.contains("run attempt 2 is a re-run"),
            "{args:?}: {stderr}"
        );
    }
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_missing_attempt_is_refused() -> Result<(), Box<dyn Error>> {
    let (code, stderr) = reconcile(&[], None)?;

    assert_eq!(code, Some(2));
    assert!(
        stderr.contains("GITHUB_RUN_ATTEMPT must be set"),
        "{stderr}"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_first_attempt_stops_at_the_missing_transport() -> Result<(), Box<dyn Error>> {
    let (code, stderr) = reconcile(&["--ledger"], Some("1"))?;

    assert_eq!(code, Some(2));
    assert!(
        stderr.contains("the ledger-side sweep runs through is not built yet (#273)"),
        "{stderr}"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn other_arguments_are_a_usage_error() -> Result<(), Box<dyn Error>> {
    let (code, stderr) = reconcile(&["--all"], Some("1"))?;

    assert_eq!(code, Some(2));
    assert!(
        stderr.contains("usage: assay reconcile [--ledger]"),
        "{stderr}"
    );
    Ok(())
}
