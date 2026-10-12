//! The pinned base cargo on a sanitised manifest-only copy (03 §2.1, 04 §2.2): what it runs,
//! where, and with which environment, against a scripted supervisor.

use std::collections::VecDeque;
use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

use assay_app::Termination;
use assay_gates::{
    CaptureSupervisor, CapturedRun, CargoEnvironment, CargoError, CargoLimits, CargoSettings,
    CargoSettingsError, CargoStep, PinnedCargo, PinnedToolchain, ProcessSpec,
};

use crate::support::SeenSpec;

const COPY: &str = "/scratch/copy";

/// A supervisor failure, scripted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CannotStart;

impl fmt::Display for CannotStart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("scripted start failure")
    }
}

impl Error for CannotStart {}

/// Plays cargo: each run takes the next scripted outcome and records what it was asked.
#[derive(Debug, Default)]
struct Scripted {
    outcomes: VecDeque<Result<CapturedRun, CannotStart>>,
    seen: Vec<SeenSpec>,
}

impl Scripted {
    fn new(outcomes: impl IntoIterator<Item = Result<CapturedRun, CannotStart>>) -> Self {
        Self {
            outcomes: outcomes.into_iter().collect(),
            seen: Vec::new(),
        }
    }
}

impl CaptureSupervisor for Scripted {
    type Error = CannotStart;

    fn capture(&mut self, spec: &ProcessSpec<'_>) -> Result<CapturedRun, CannotStart> {
        self.seen.push(SeenSpec {
            program: spec.program.to_path_buf(),
            args: spec.args.to_vec(),
            cwd: spec.cwd.to_path_buf(),
            env: spec.env.to_vec(),
            timeout: spec.timeout,
            max_output_bytes: spec.max_output_bytes,
        });
        self.outcomes.pop_front().unwrap_or(Err(CannotStart))
    }
}

/// A scripted run that exited with `code` after printing `stdout`.
fn exited(code: i32, stdout: &[u8]) -> CapturedRun {
    CapturedRun {
        termination: Termination::Exited(code),
        stdout: stdout.to_vec(),
    }
}

fn vars(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    pairs
        .iter()
        .map(|(name, value)| (OsString::from(name), OsString::from(value)))
        .collect()
}

fn toolchain() -> PinnedToolchain {
    PinnedToolchain {
        cargo: PathBuf::from("/opt/rust/bin/cargo"),
        rustc: PathBuf::from("/opt/rust/bin/rustc"),
        name: "1.98.1".to_owned(),
    }
}

fn settings() -> Result<CargoSettings, CargoSettingsError> {
    CargoSettings::new(
        toolchain(),
        "/scratch/cargo-home",
        CargoEnvironment::from_vars(vars(&[("PATH", "/usr/bin:/bin"), ("HOME", "/home/ci")])),
        CargoLimits::DEFAULT,
    )
}

#[test]
fn runs_fetch_then_metadata_in_the_copy_with_the_pinned_toolchain() -> Result<(), Box<dyn Error>> {
    let mut supervisor = Scripted::new([Ok(exited(0, b"")), Ok(exited(0, b"{\"version\":1}"))]);

    let metadata = PinnedCargo::new(&mut supervisor, settings()?).metadata(Path::new(COPY))?;

    let manifest = format!("{COPY}/Cargo.toml");
    let expected_env = vars(&[
        ("PATH", "/usr/bin:/bin"),
        ("HOME", "/home/ci"),
        ("CARGO_HOME", "/scratch/cargo-home"),
        ("RUSTUP_TOOLCHAIN", "1.98.1"),
        ("RUSTC", "/opt/rust/bin/rustc"),
        ("CARGO_TERM_COLOR", "never"),
    ]);
    let run = |args: &[&str], timeout| SeenSpec {
        program: PathBuf::from("/opt/rust/bin/cargo"),
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        cwd: PathBuf::from(COPY),
        env: expected_env.clone(),
        timeout,
        max_output_bytes: CargoLimits::DEFAULT.max_output_bytes,
    };
    assert_eq!(
        (metadata.as_str(), supervisor.seen),
        (
            "{\"version\":1}",
            vec![
                run(
                    &[
                        "fetch",
                        "--locked",
                        "--manifest-path",
                        &manifest,
                        "--config",
                        "net.git-fetch-with-cli=false",
                    ],
                    CargoLimits::DEFAULT.fetch_timeout,
                ),
                run(
                    &[
                        "metadata",
                        "--locked",
                        "--offline",
                        "--format-version",
                        "1",
                        "--all-features",
                        "--manifest-path",
                        &manifest,
                        "--config",
                        "net.git-fetch-with-cli=false",
                    ],
                    CargoLimits::DEFAULT.metadata_timeout,
                ),
            ]
        ),
        "the pinned cargo and rustc, explicit CARGO_HOME and toolchain, nothing else inherited"
    );
    Ok(())
}

#[test]
fn inherited_cargo_and_rust_variables_never_reach_cargo() {
    let inherited = vars(&[
        ("PATH", "/usr/bin"),
        ("CARGO_HOME", "/home/ci/.cargo"),
        ("RUSTUP_TOOLCHAIN", "nightly"),
        ("RUSTC", "/evil/rustc"),
        ("RUSTC_WRAPPER", "/evil/wrapper"),
        ("RUSTFLAGS", "-Cevil"),
        ("CARGO_BUILD_RUSTC", "/evil/rustc"),
        ("CARGO_REGISTRIES_X_INDEX", "https://evil.invalid"),
        ("GITHUB_TOKEN", "secret"),
        ("RUSTUP_HOME", "/home/ci/.rustup"),
        ("HTTPS_PROXY", "http://proxy:3128"),
        ("HOME", "/home/ci"),
    ]);

    let environment = CargoEnvironment::from_vars(inherited);

    assert_eq!(
        environment.vars(),
        vars(&[
            ("PATH", "/usr/bin"),
            ("RUSTUP_HOME", "/home/ci/.rustup"),
            ("HTTPS_PROXY", "http://proxy:3128"),
            ("HOME", "/home/ci"),
        ]),
        "cargo's and rustc's own settings come only from the adapter"
    );
}

#[test]
fn a_failed_fetch_stops_before_metadata() -> Result<(), Box<dyn Error>> {
    let mut supervisor = Scripted::new([Ok(exited(101, b""))]);

    let result = PinnedCargo::new(&mut supervisor, settings()?).metadata(Path::new(COPY));

    assert!(
        matches!(
            result,
            Err(CargoError::Failed {
                step: CargoStep::Fetch,
                termination: Termination::Exited(101),
            })
        ),
        "got {result:?}"
    );
    assert_eq!(supervisor.seen.len(), 1, "metadata never runs");
    Ok(())
}

#[test]
fn a_metadata_run_past_its_deadline_is_an_error() -> Result<(), Box<dyn Error>> {
    let mut supervisor = Scripted::new([
        Ok(exited(0, b"")),
        Ok(CapturedRun {
            termination: Termination::TimedOut,
            stdout: Vec::new(),
        }),
    ]);

    let result = PinnedCargo::new(&mut supervisor, settings()?).metadata(Path::new(COPY));

    assert!(
        matches!(
            result,
            Err(CargoError::Failed {
                step: CargoStep::Metadata,
                termination: Termination::TimedOut,
            })
        ),
        "got {result:?}"
    );
    Ok(())
}

#[test]
fn metadata_that_is_not_utf8_is_an_error() -> Result<(), Box<dyn Error>> {
    let mut supervisor = Scripted::new([Ok(exited(0, b"")), Ok(exited(0, b"{\"x\":\"\xff\"}"))]);

    let result = PinnedCargo::new(&mut supervisor, settings()?).metadata(Path::new(COPY));

    assert!(matches!(result, Err(CargoError::NotUtf8)), "got {result:?}");
    Ok(())
}

#[test]
fn a_supervisor_failure_is_an_error() -> Result<(), Box<dyn Error>> {
    let mut supervisor = Scripted::new([Err(CannotStart)]);

    let result = PinnedCargo::new(&mut supervisor, settings()?).metadata(Path::new(COPY));

    assert!(
        matches!(
            result,
            Err(CargoError::Supervisor {
                step: CargoStep::Fetch,
                source: CannotStart,
            })
        ),
        "got {result:?}"
    );
    Ok(())
}

#[test]
fn a_relative_copy_is_refused_before_anything_runs() -> Result<(), Box<dyn Error>> {
    let mut supervisor = Scripted::default();

    let result = PinnedCargo::new(&mut supervisor, settings()?).metadata(Path::new("copy"));

    assert!(
        matches!(result, Err(CargoError::CopyPath { .. })),
        "got {result:?}"
    );
    assert_eq!(supervisor.seen, Vec::new(), "nothing runs");
    Ok(())
}

#[test]
fn settings_refuse_relative_paths_and_an_empty_toolchain() {
    let environment = || CargoEnvironment::from_vars(Vec::new());
    let with = |toolchain: PinnedToolchain, home: &str| {
        CargoSettings::new(toolchain, home, environment(), CargoLimits::DEFAULT).err()
    };

    let refused = [
        with(
            PinnedToolchain {
                cargo: PathBuf::from("cargo"),
                ..toolchain()
            },
            "/home",
        ),
        with(
            PinnedToolchain {
                rustc: PathBuf::from("bin/rustc"),
                ..toolchain()
            },
            "/home",
        ),
        with(
            PinnedToolchain {
                name: String::new(),
                ..toolchain()
            },
            "/home",
        ),
        with(toolchain(), ".cargo"),
    ];

    assert_eq!(
        refused,
        [
            Some(CargoSettingsError::RelativeCargo {
                path: PathBuf::from("cargo")
            }),
            Some(CargoSettingsError::RelativeRustc {
                path: PathBuf::from("bin/rustc")
            }),
            Some(CargoSettingsError::EmptyToolchain),
            Some(CargoSettingsError::RelativeCargoHome {
                path: PathBuf::from(".cargo")
            }),
        ]
    );
}
