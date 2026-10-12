//! `assay self-gate` as the `assay-self` exec gate (02 §7, 05 T16), run end to end through the
//! exec adapter, `ForgeSupervisor` and `run_gates`, so its document is checked against the gate
//! contract (03 §2) exactly as a declared `exec` plugin's would be.

use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use assay_app::{
    BuildJob, Conclusion, ContractValidator, GateError, GateJob, GateOutcome, PluginsConfig,
    run_gates,
};
use assay_cli::ForgeSupervisor;
use assay_forge::process::GroupKiller;
use assay_gates::{ExecAdapter, ExecLimits, ExecSettings, GateEnvironment};
use tempfile::TempDir;

type TestResult = Result<(), Box<dyn Error>>;

fn repo_root() -> Result<PathBuf, Box<dyn Error>> {
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?)
}

/// The `[[plugin]]` entry 02 §7 documents, with the `assay` under test as the program.
fn config() -> Result<PluginsConfig, Box<dyn Error>> {
    let text = format!(
        r#"
[assay]
digest = "sha256:{zeros}"

[[plugin]]
name = "assay-self"
adapter = "exec"
exec = ["{bin}", "self-gate", "corpus/golden", "rubric/v1.toml"]
job = "test"
required = true
timeout_s = 600
"#,
        zeros = "0".repeat(64),
        bin = env!("CARGO_BIN_EXE_assay"),
    );
    Ok(text.parse()?)
}

/// The run key variable a gate job passes on.
fn run_key() -> Vec<(OsString, OsString)> {
    vec![("ASSAY_RUN_KEY".into(), "sha256:self-gate-test".into())]
}

/// The outcome of the gate run from `checkout` with `vars` as the inherited environment.
fn run_gate(
    checkout: &Path,
    vars: Vec<(OsString, OsString)>,
) -> Result<GateOutcome, Box<dyn Error>> {
    let output = TempDir::new()?;
    let settings = ExecSettings::new(
        checkout,
        output.path(),
        GateEnvironment::from_vars(vars),
        ExecLimits::DEFAULT,
    )?;
    let mut adapter = ExecAdapter::new(ForgeSupervisor::new(GroupKiller::system()?), settings);
    let reports = run_gates(
        &config()?,
        GateJob::Build(BuildJob::Test),
        &mut adapter,
        &ContractValidator::new()?,
    );
    let [report] = reports.as_slice() else {
        return Err(format!("one report expected, got {reports:?}").into());
    };
    Ok(report.outcome)
}

/// Copies the directory `from` into `to`, recursively.
fn copy_dir(from: &Path, to: &Path) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// A checkout holding the recorded corpus and `rubric/v1.toml` with its rule-7 threshold
/// raised: a rubric change without a snapshot update.
fn rubric_changed_checkout() -> Result<TempDir, Box<dyn Error>> {
    let root = repo_root()?;
    let checkout = TempDir::new()?;
    copy_dir(
        &root.join("corpus/golden"),
        &checkout.path().join("corpus/golden"),
    )?;
    let rubric = fs::read_to_string(root.join("rubric/v1.toml"))?;
    let line = rubric
        .lines()
        .find(|line| line.starts_with("majors_block_threshold"))
        .ok_or("rubric/v1.toml sets majors_block_threshold")?;
    fs::create_dir_all(checkout.path().join("rubric"))?;
    fs::write(
        checkout.path().join("rubric/v1.toml"),
        rubric.replacen(line, "majors_block_threshold = 97", 1),
    )?;
    Ok(checkout)
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn the_gate_passes_on_this_repository() -> TestResult {
    let outcome = run_gate(&repo_root()?, run_key())?;

    assert_eq!(
        outcome,
        GateOutcome::Concluded(Conclusion::Pass),
        "every recorded snapshot replays to its bytes"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_rubric_change_without_a_snapshot_update_fails_the_gate() -> TestResult {
    let checkout = rubric_changed_checkout()?;

    let outcome = run_gate(checkout.path(), run_key())?;

    assert_eq!(
        outcome,
        GateOutcome::Concluded(Conclusion::Fail),
        "a verdict change without a snapshot update fails (rule 1)"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn without_a_run_key_the_gate_is_a_gate_error() -> TestResult {
    let outcome = run_gate(&repo_root()?, Vec::new())?;

    assert_eq!(
        outcome,
        GateOutcome::Error(GateError::Exit(2)),
        "no document can name its run, so nothing is written and nothing passes"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_missing_corpus_is_a_gate_error() -> TestResult {
    let checkout = TempDir::new()?;
    let root = repo_root()?;
    fs::create_dir_all(checkout.path().join("rubric"))?;
    fs::copy(
        root.join("rubric/v1.toml"),
        checkout.path().join("rubric/v1.toml"),
    )?;

    let outcome = run_gate(checkout.path(), run_key())?;

    assert_eq!(
        outcome,
        GateOutcome::Error(GateError::Exit(2)),
        "an unreadable corpus is never a pass"
    );
    Ok(())
}
