//! The `assay` binary exists under that name and runs.

use std::error::Error;
use std::path::Path;
use std::process::{Command, Stdio};

use tempfile::TempDir;

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn binary_is_named_assay_and_exits_successfully() -> Result<(), std::io::Error> {
    let status = Command::new(env!("CARGO_BIN_EXE_assay")).status()?;

    assert_eq!(
        status.code(),
        Some(0),
        "`assay` with no arguments must exit 0"
    );
    Ok(())
}

/// The exit code of `assay` run with `args` from the repository root, with `ASSAY_RUN_KEY` set
/// and `ASSAY_FINDINGS_OUT` set to `findings_out`.
fn exit_code(args: &[&str], findings_out: &str) -> Result<Option<i32>, std::io::Error> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let status = Command::new(env!("CARGO_BIN_EXE_assay"))
        .args(args)
        .current_dir(root)
        .env("ASSAY_RUN_KEY", "sha256:binary-test")
        .env("ASSAY_FINDINGS_OUT", findings_out)
        .stderr(Stdio::null())
        .status()?;
    Ok(status.code())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn usage_errors_exit_2_and_write_nothing() -> Result<(), Box<dyn Error>> {
    let out = TempDir::new()?;
    let findings = out.path().join("findings.json");
    let findings_out = findings.to_str().ok_or("a UTF-8 temporary path")?;
    let gate = ["self-gate", "corpus/golden", "rubric/v1.toml"];

    let codes = [
        exit_code(&["no-such-command"], findings_out)?,
        exit_code(&["self-gate", "corpus/golden"], findings_out)?,
        exit_code(&[&gate[..], &["extra"]].concat(), findings_out)?,
        exit_code(&gate, "")?,
    ];

    assert_eq!(
        (codes, findings.exists()),
        ([Some(2); 4], false),
        "an unknown subcommand, one or three arguments and an empty ASSAY_FINDINGS_OUT are \
         gate errors"
    );
    Ok(())
}
