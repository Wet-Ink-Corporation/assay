//! The steps-only temper adapter (#67, `docs/architecture/03-contracts.md` §2.1 step 7): what
//! `gates-test` runs on its own VM, with nothing of `gates-lint` (no node, no pinned temper, no
//! object-only repository, no ratchet base). It runs the declared `test` steps exactly as the
//! full temper adapter does, and refuses every other job before anything runs.

use std::error::Error;
use std::ffi::OsString;
use std::path::PathBuf;

use assay_app::{
    BuildJob, ContractValidator, GateJob, GateJobReport, GateRunner, Termination, run_gates,
};
use assay_gates::{
    GateEnvironment, TemperAdapter, TemperError, TemperSettingsError, TemperStepSettings,
    TemperTestAdapter,
};

use crate::support::plugin;
use crate::temper_support::Pr;
use crate::temper_test::{FakeSteps, config};

const TEST: GateJob = GateJob::Build(BuildJob::Test);
const SIGNALLED: Termination = Termination::Signalled;

/// The environment `Pr::settings` gives the full adapter.
fn environment() -> GateEnvironment {
    GateEnvironment::from_vars([
        (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
        (OsString::from("GITHUB_TOKEN"), OsString::from("secret")),
    ])
}

/// One adapter's gates-test: its reports and the supervisor that saw its processes.
type Ran = (Vec<GateJobReport>, FakeSteps);

/// gates-test through the gate use case with the full adapter, then with the steps-only one,
/// each over its own copy of `script`: their reports and the processes each asked for.
fn both(script: &FakeSteps) -> Result<[Ran; 2], Box<dyn Error>> {
    let pr = Pr::new()?;
    let config = config()?;
    let validator = ContractValidator::new()?;
    let mut full = script.clone();
    let mut adapter = TemperAdapter::new(&mut full, &pr.revisions, pr.settings()?);
    let full_reports = run_gates(&config, TEST, &mut adapter, &validator);
    let mut only = script.clone();
    let settings = TemperStepSettings::new(pr.head_tree.clone(), &environment(), "run-1")?;
    let mut adapter = TemperTestAdapter::new(&mut only, settings);
    let only_reports = run_gates(&config, TEST, &mut adapter, &validator);
    Ok([(full_reports, full), (only_reports, only)])
}

#[test]
fn it_runs_gates_test_exactly_as_the_temper_adapter_does() -> Result<(), Box<dyn Error>> {
    let scripts = [
        FakeSteps::default(),
        FakeSteps::default().ends("nextest", &[Termination::Exited(4)]),
        FakeSteps::default().ends("test", &[SIGNALLED, Termination::Exited(0)]),
        FakeSteps::default().ends("nextest", &[SIGNALLED, SIGNALLED]),
        FakeSteps::default().ends("nextest", &[Termination::TimedOut]),
    ];
    for script in scripts {
        let [(full, full_steps), (only, only_steps)] = both(&script)?;

        assert_eq!(only, full, "{:?}", script.ran());
        assert_eq!(only_steps.seen, full_steps.seen, "{:?}", script.ran());
    }
    Ok(())
}

#[test]
fn it_refuses_gates_static_and_gates_lint_before_running_anything() -> Result<(), Box<dyn Error>> {
    let pr = Pr::new()?;
    let config = config()?;
    let temper = plugin(&config, "temper")?;
    for job in [GateJob::Static, GateJob::Build(BuildJob::Lint)] {
        let mut steps = FakeSteps::default();
        let settings = TemperStepSettings::new(pr.head_tree.clone(), &environment(), "run-1")?;
        let mut adapter = TemperTestAdapter::new(&mut steps, settings);

        let ran = adapter.run(&temper, job);

        assert!(
            matches!(ran, Err(TemperError::UnsupportedJob { job: refused }) if refused == job),
            "{job}: {ran:?}"
        );
        assert!(steps.seen.is_empty(), "{job}: nothing runs");
    }
    Ok(())
}

#[test]
fn its_settings_refuse_what_the_full_settings_refuse() {
    let relative_path =
        GateEnvironment::from_vars([(OsString::from("PATH"), OsString::from("bin:/usr/bin"))]);
    let cases = [
        (PathBuf::from("head"), environment(), "run-1"),
        (PathBuf::from("/"), environment(), "run-1"),
        (PathBuf::from("/work/head"), relative_path, "run-1"),
        (PathBuf::from("/work/head"), environment(), ""),
    ];

    let refused: Vec<Option<TemperSettingsError>> = cases
        .into_iter()
        .map(|(head, environment, run_key)| {
            TemperStepSettings::new(head, &environment, run_key).err()
        })
        .collect();

    assert!(
        matches!(
            refused.as_slice(),
            [
                Some(TemperSettingsError::HeadTree { .. }),
                Some(TemperSettingsError::HeadTree { .. }),
                Some(TemperSettingsError::RelativeSearchPath),
                Some(TemperSettingsError::EmptyRunKey),
            ]
        ),
        "{refused:?}"
    );
}
