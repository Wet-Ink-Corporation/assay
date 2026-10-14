//! The `exec` adapter: a PR-controlled command run in a Z2b job with a scrubbed environment,
//! bounded by the plugin's timeout and the output caps (`docs/architecture/03-contracts.md` §1,
//! §2).

use std::assert_matches;
use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use assay_app::{
    BuildJob, ContractValidator, GateDocument, GateJob, GateJobOutcome, GateRun, GateRunner,
    Termination, run_gates,
};
use assay_domain::GateConclusion;
use assay_gates::{
    ExecAdapter, ExecError, ExecLimits, ExecSettings, ExecSettingsError, GateEnvironment,
};
use tempfile::TempDir;

use crate::support::{FakeSpawnError, FakeSupervisor, Leaves, config, document, plugin};

const LINT: GateJob = GateJob::Build(BuildJob::Lint);
const LIMITS: ExecLimits = ExecLimits {
    max_output_bytes: 4096,
    max_document_bytes: 1024,
};

/// A head checkout and an output root in one temporary directory.
struct Dirs {
    _root: TempDir,
    checkout: PathBuf,
    output_root: PathBuf,
}

impl Dirs {
    fn new() -> Result<Self, Box<dyn Error>> {
        let root = tempfile::tempdir()?;
        let checkout = root.path().join("checkout");
        let output_root = root.path().join("out");
        fs::create_dir(&checkout)?;
        fs::create_dir(&output_root)?;
        Ok(Self {
            _root: root,
            checkout,
            output_root,
        })
    }

    fn settings(&self, inherited: &[(&str, &str)]) -> Result<ExecSettings, ExecSettingsError> {
        ExecSettings::new(
            &self.checkout,
            &self.output_root,
            GateEnvironment::from_vars(
                inherited
                    .iter()
                    .map(|(name, value)| (OsString::from(name), OsString::from(value))),
            ),
            LIMITS,
        )
    }

    fn findings_path(&self) -> PathBuf {
        self.output_root.join("custom-lint").join("findings.json")
    }
}

/// Runs `custom-lint` in `job` once with `supervisor`.
fn run_exec(
    dirs: &Dirs,
    supervisor: &mut FakeSupervisor,
    job: GateJob,
) -> Result<Result<GateRun, ExecError<FakeSpawnError>>, Box<dyn Error>> {
    let config = config()?;
    let custom = plugin(&config, "custom-lint")?;
    let mut adapter = ExecAdapter::new(supervisor, dirs.settings(&[("PATH", "/bin")])?);
    Ok(adapter.run(&custom, job))
}

#[test]
fn passes_only_the_allowlist_and_the_findings_path_to_the_process() -> Result<(), Box<dyn Error>> {
    let dirs = Dirs::new()?;
    let config = config()?;
    let custom = plugin(&config, "custom-lint")?;
    let mut supervisor = FakeSupervisor::new(Termination::Exited(0), Leaves::Nothing);
    let mut adapter = ExecAdapter::new(
        &mut supervisor,
        dirs.settings(&[
            ("GITHUB_TOKEN", "ghs_secret"),
            ("PATH", "/usr/bin"),
            ("HOME", "/home/runner"),
            ("CI", "true"),
            ("ASSAY_RUN_KEY", "k"),
        ])?,
    );

    adapter.run(&custom, LINT)?;

    let env: Vec<(OsString, OsString)> = supervisor
        .seen
        .iter()
        .flat_map(|seen| seen.env.iter().cloned())
        .collect();
    assert_eq!(
        env,
        [
            ("PATH".into(), "/usr/bin".into()),
            ("HOME".into(), "/home/runner".into()),
            ("CI".into(), "true".into()),
            ("ASSAY_RUN_KEY".into(), "k".into()),
            ("ASSAY_FINDINGS_OUT".into(), dirs.findings_path().into()),
        ]
    );
    Ok(())
}

#[test]
fn an_inherited_findings_path_cannot_redirect_the_document() -> Result<(), Box<dyn Error>> {
    let dirs = Dirs::new()?;
    let config = config()?;
    let custom = plugin(&config, "custom-lint")?;
    let mut supervisor = FakeSupervisor::new(Termination::Exited(0), Leaves::Nothing);
    let mut adapter = ExecAdapter::new(
        &mut supervisor,
        dirs.settings(&[("ASSAY_FINDINGS_OUT", "/tmp/elsewhere.json")])?,
    );

    adapter.run(&custom, LINT)?;

    let paths: Vec<&OsString> = supervisor
        .seen
        .iter()
        .flat_map(|seen| seen.env.iter())
        .filter(|(name, _)| name == "ASSAY_FINDINGS_OUT")
        .map(|(_, value)| value)
        .collect();
    assert_eq!(paths, [&OsString::from(dirs.findings_path())]);
    Ok(())
}

#[test]
fn runs_the_command_from_the_checkout_within_the_plugins_limits() -> Result<(), Box<dyn Error>> {
    let dirs = Dirs::new()?;
    let mut supervisor = FakeSupervisor::new(Termination::Exited(0), Leaves::Nothing);

    run_exec(&dirs, &mut supervisor, LINT)??;

    let seen: Vec<(PathBuf, Vec<String>, PathBuf, Duration, usize)> = supervisor
        .seen
        .iter()
        .map(|seen| {
            (
                seen.program.clone(),
                seen.args.clone(),
                seen.cwd.clone(),
                seen.timeout,
                seen.max_output_bytes,
            )
        })
        .collect();
    assert_eq!(
        seen,
        [(
            dirs.checkout.join("./scripts/lint.sh"),
            vec!["--json".to_owned()],
            dirs.checkout.clone(),
            Duration::from_secs(90),
            LIMITS.max_output_bytes,
        )]
    );
    Ok(())
}

#[test]
fn reports_the_termination_and_the_written_document() -> Result<(), Box<dyn Error>> {
    let dirs = Dirs::new()?;
    let written = document("custom-lint", "fail");
    let mut supervisor = FakeSupervisor::new(Termination::Exited(1), Leaves::File(written.clone()));

    let run = run_exec(&dirs, &mut supervisor, LINT)??;

    assert_eq!(
        run,
        GateRun {
            termination: Termination::Exited(1),
            document: GateDocument::Written(written),
        }
    );
    Ok(())
}

#[test]
fn reports_a_missing_document() -> Result<(), Box<dyn Error>> {
    let dirs = Dirs::new()?;
    let mut supervisor = FakeSupervisor::new(Termination::Exited(0), Leaves::Nothing);

    let run = run_exec(&dirs, &mut supervisor, LINT)??;

    assert_eq!(run.document, GateDocument::Missing);
    Ok(())
}

/// A document the default cap lets through is never refused for its size when it is read as a
/// contract.
#[test]
fn default_document_cap_is_the_contract_read_cap() {
    assert_eq!(
        ExecLimits::DEFAULT.max_document_bytes,
        assay_app::MAX_DOCUMENT_BYTES
    );
}

#[test]
fn does_not_read_a_document_over_the_cap() -> Result<(), Box<dyn Error>> {
    let dirs = Dirs::new()?;
    let cap = LIMITS.max_document_bytes;
    let mut supervisor =
        FakeSupervisor::new(Termination::Exited(0), Leaves::File(vec![b' '; cap + 1]));

    let run = run_exec(&dirs, &mut supervisor, LINT)??;

    assert_eq!(run.document, GateDocument::TooLarge);
    Ok(())
}

#[test]
fn reads_a_document_exactly_at_the_cap() -> Result<(), Box<dyn Error>> {
    let dirs = Dirs::new()?;
    let cap = LIMITS.max_document_bytes;
    let mut supervisor = FakeSupervisor::new(Termination::Exited(0), Leaves::File(vec![b' '; cap]));

    let run = run_exec(&dirs, &mut supervisor, LINT)??;

    assert_eq!(run.document, GateDocument::Written(vec![b' '; cap]));
    Ok(())
}

#[test]
fn does_not_follow_a_symlinked_document() -> Result<(), Box<dyn Error>> {
    let dirs = Dirs::new()?;
    let target = dirs.checkout.join("elsewhere.json");
    fs::write(&target, document("custom-lint", "pass"))?;
    let mut supervisor = FakeSupervisor::new(Termination::Exited(0), Leaves::SymlinkTo(target));

    let run = run_exec(&dirs, &mut supervisor, LINT)??;

    assert_eq!(run.document, GateDocument::NotAFile);
    Ok(())
}

#[test]
fn refuses_to_run_in_the_static_job() -> Result<(), Box<dyn Error>> {
    let dirs = Dirs::new()?;
    let mut supervisor = FakeSupervisor::new(Termination::Exited(0), Leaves::Nothing);

    let result = run_exec(&dirs, &mut supervisor, GateJob::Static)?;

    assert!(
        matches!(result, Err(ExecError::OutsideBuildJob)),
        "{result:?}"
    );
    assert!(supervisor.seen.is_empty(), "nothing may run in Z2s");
    Ok(())
}

#[test]
fn refuses_to_run_in_another_build_job() -> Result<(), Box<dyn Error>> {
    let dirs = Dirs::new()?;
    let mut supervisor = FakeSupervisor::new(Termination::Exited(0), Leaves::Nothing);

    let result = run_exec(&dirs, &mut supervisor, GateJob::Build(BuildJob::Test))?;

    assert!(
        matches!(
            result,
            Err(ExecError::WrongJob {
                job: BuildJob::Test,
                ..
            })
        ),
        "{result:?}"
    );
    assert!(supervisor.seen.is_empty(), "nothing may run");
    Ok(())
}

#[test]
fn refuses_a_plugin_with_another_adapter() -> Result<(), Box<dyn Error>> {
    let dirs = Dirs::new()?;
    let config = config()?;
    let temper = plugin(&config, "temper")?;
    let mut supervisor = FakeSupervisor::new(Termination::Exited(0), Leaves::Nothing);
    let mut adapter = ExecAdapter::new(&mut supervisor, dirs.settings(&[])?);

    let result = adapter.run(&temper, LINT);

    assert!(
        matches!(result, Err(ExecError::NotExec { ref plugin }) if plugin == "temper"),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn refuses_an_output_directory_that_already_exists() -> Result<(), Box<dyn Error>> {
    let dirs = Dirs::new()?;
    fs::create_dir(dirs.output_root.join("custom-lint"))?;
    fs::write(dirs.findings_path(), document("custom-lint", "pass"))?;
    let mut supervisor = FakeSupervisor::new(Termination::Exited(0), Leaves::Nothing);

    let result = run_exec(&dirs, &mut supervisor, LINT)?;

    assert!(
        matches!(result, Err(ExecError::OutputDirectory { .. })),
        "{result:?}"
    );
    assert!(
        supervisor.seen.is_empty(),
        "a stale document is never believed"
    );
    Ok(())
}

#[test]
fn a_supervisor_failure_is_an_exec_error() -> Result<(), Box<dyn Error>> {
    let dirs = Dirs::new()?;
    let config = config()?;
    let custom = plugin(&config, "custom-lint")?;
    let mut supervisor = FakeSupervisor::new(Termination::Exited(0), Leaves::CannotStart);
    let mut adapter = ExecAdapter::new(&mut supervisor, dirs.settings(&[])?);

    let result = adapter.run(&custom, LINT);

    assert!(
        matches!(
            result,
            Err(ExecError::Supervisor(FakeSpawnError::CannotStart))
        ),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn settings_refuse_a_relative_checkout() {
    let settings = ExecSettings::new(
        "checkout",
        "/runner/out",
        GateEnvironment::default(),
        LIMITS,
    );

    assert_matches!(
        settings,
        Err(ExecSettingsError::RelativeCheckout { path }) if path == Path::new("checkout")
    );
}

#[test]
fn settings_refuse_a_relative_output_root() {
    let settings = ExecSettings::new(
        "/runner/checkout",
        "out",
        GateEnvironment::default(),
        LIMITS,
    );

    assert_matches!(
        settings,
        Err(ExecSettingsError::RelativeOutputRoot { path }) if path == Path::new("out")
    );
}

#[test]
fn a_wrong_job_is_named_as_its_gate_job() {
    let error: ExecError<FakeSpawnError> = ExecError::WrongJob {
        plugin: "custom-lint".to_owned(),
        job: BuildJob::Test,
    };

    assert_eq!(
        error.to_string(),
        "plugin `custom-lint` does not run in gates-test"
    );
}

#[test]
fn a_clean_exec_run_passes_through_the_gate_use_case() -> Result<(), Box<dyn Error>> {
    let dirs = Dirs::new()?;
    let config = config()?;
    let validator = ContractValidator::new()?;
    let mut supervisor = FakeSupervisor::new(
        Termination::Exited(0),
        Leaves::File(document("custom-lint", "pass")),
    );
    let mut adapter = ExecAdapter::new(&mut supervisor, dirs.settings(&[])?);

    let reports = run_gates(&config, LINT, &mut adapter, &validator);

    let outcomes: Vec<(&str, GateJobOutcome)> = reports
        .iter()
        .map(|report| (report.plugin.as_str(), report.outcome))
        .collect();
    assert_eq!(
        outcomes,
        [
            (
                "temper",
                GateJobOutcome::Error(assay_app::GateError::NotRun)
            ),
            (
                "custom-lint",
                GateJobOutcome::Concluded(GateConclusion::Pass)
            ),
        ]
    );
    Ok(())
}
