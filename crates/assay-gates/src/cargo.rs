//! The pinned base cargo on a sanitised manifest-only copy (`docs/architecture/03-contracts.md`
//! §2.1, `04-pipeline.md` §2.2): `cargo fetch --locked`, then `cargo metadata --locked
//! --offline`, whose output `assay_app::PackageSet::from_metadata` reads.
//!
//! Nothing from the PR configures it: the copy holds no `.cargo/` directory, toolchain file or
//! `.gitattributes`, the environment is cleared to [`CargoEnvironment`] plus the explicit
//! `CARGO_HOME`, `RUSTUP_TOOLCHAIN` and `RUSTC`, and git dependencies are fetched without the
//! system git (`--config net.git-fetch-with-cli=false`).

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use assay_app::Termination;
use thiserror::Error;

use crate::capture::{CaptureSupervisor, CapturedRun};
use crate::exec::ProcessSpec;

/// The inherited variables cargo may see: where programs and the toolchains live, and the
/// proxy settings a registry fetch may need.
const ALLOWED: [&str; 7] = [
    "PATH",
    "HOME",
    "RUSTUP_HOME",
    "HTTPS_PROXY",
    "https_proxy",
    "NO_PROXY",
    "no_proxy",
];

/// `--config` on every run: git dependencies are fetched by cargo's own git, never by a
/// system git whose configuration assay does not control.
const CONFIG: [&str; 2] = ["--config", "net.git-fetch-with-cli=false"];

/// The pinned base toolchain (plain data; [`CargoSettings::new`] validates it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedToolchain {
    /// The cargo binary, an absolute path.
    pub cargo: PathBuf,
    /// The rustc binary, an absolute path, set as `RUSTC` (`cargo metadata` runs `rustc -vV`).
    pub rustc: PathBuf,
    /// The toolchain name set as `RUSTUP_TOOLCHAIN`, so a rustup proxy never reads a toolchain
    /// file.
    pub name: String,
}

/// The inherited variables cargo may see: `PATH`, `HOME`, `RUSTUP_HOME` and the proxy
/// settings (`HTTPS_PROXY`, `https_proxy`, `NO_PROXY`, `no_proxy`).
///
/// Names match exactly. Every other variable is dropped, cargo's and rustc's own (`CARGO_*`,
/// `RUSTC*`, `RUSTFLAGS`, `RUSTUP_TOOLCHAIN`) and tokens included. Capture it once at the
/// composition root from `std::env::vars_os()`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CargoEnvironment {
    vars: Vec<(OsString, OsString)>,
}

impl CargoEnvironment {
    /// Keeps the allowlisted variables of `vars`, in their order.
    #[must_use]
    pub fn from_vars(vars: impl IntoIterator<Item = (OsString, OsString)>) -> Self {
        Self {
            vars: vars.into_iter().filter(|(name, _)| allowed(name)).collect(),
        }
    }

    /// The kept variables, as name and value.
    #[must_use]
    pub fn vars(&self) -> &[(OsString, OsString)] {
        &self.vars
    }
}

/// Whether a variable named `name` may reach cargo.
fn allowed(name: &OsStr) -> bool {
    name.to_str().is_some_and(|name| ALLOWED.contains(&name))
}

/// Bounds on the two cargo runs (H-13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CargoLimits {
    /// Wall-clock limit of `cargo fetch`, which downloads from the registries.
    pub fetch_timeout: Duration,
    /// Wall-clock limit of `cargo metadata`, which runs offline.
    pub metadata_timeout: Duration,
    /// Most bytes accepted on stdout, and separately stderr, of each run.
    pub max_output_bytes: usize,
}

impl CargoLimits {
    /// Ten minutes to fetch, two for metadata, and 64 MiB of output.
    pub const DEFAULT: Self = Self {
        fetch_timeout: Duration::from_secs(600),
        metadata_timeout: Duration::from_secs(120),
        max_output_bytes: 64 << 20,
    };
}

/// How the pinned cargo runs in this job.
#[derive(Debug, Clone)]
pub struct CargoSettings {
    toolchain: PinnedToolchain,
    cargo_home: PathBuf,
    environment: CargoEnvironment,
    limits: CargoLimits,
}

/// Why [`CargoSettings`] were refused.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CargoSettingsError {
    /// The cargo binary is a relative path, which would resolve against the copy.
    #[error("the cargo binary {} is not an absolute path", path.display())]
    RelativeCargo {
        /// The refused path.
        path: PathBuf,
    },
    /// The rustc binary is a relative path.
    #[error("the rustc binary {} is not an absolute path", path.display())]
    RelativeRustc {
        /// The refused path.
        path: PathBuf,
    },
    /// The toolchain name is empty, so `RUSTUP_TOOLCHAIN` would pin nothing.
    #[error("the toolchain name is empty")]
    EmptyToolchain,
    /// `CARGO_HOME` is a relative path.
    #[error("CARGO_HOME {} is not an absolute path", path.display())]
    RelativeCargoHome {
        /// The refused path.
        path: PathBuf,
    },
}

impl CargoSettings {
    /// Settings that run `toolchain`'s cargo with `CARGO_HOME` at `cargo_home` (a directory
    /// assay owns, never one the PR could have written), `environment` and `limits`.
    ///
    /// # Errors
    ///
    /// [`CargoSettingsError`] when a binary or `cargo_home` is a relative path, or the
    /// toolchain name is empty.
    pub fn new(
        toolchain: PinnedToolchain,
        cargo_home: impl Into<PathBuf>,
        environment: CargoEnvironment,
        limits: CargoLimits,
    ) -> Result<Self, CargoSettingsError> {
        if !toolchain.cargo.is_absolute() {
            return Err(CargoSettingsError::RelativeCargo {
                path: toolchain.cargo,
            });
        }
        if !toolchain.rustc.is_absolute() {
            return Err(CargoSettingsError::RelativeRustc {
                path: toolchain.rustc,
            });
        }
        if toolchain.name.is_empty() {
            return Err(CargoSettingsError::EmptyToolchain);
        }
        let cargo_home = cargo_home.into();
        if !cargo_home.is_absolute() {
            return Err(CargoSettingsError::RelativeCargoHome { path: cargo_home });
        }
        Ok(Self {
            toolchain,
            cargo_home,
            environment,
            limits,
        })
    }
}

/// Which cargo run failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CargoStep {
    /// `cargo fetch --locked`.
    Fetch,
    /// `cargo metadata --locked --offline`.
    Metadata,
}

impl fmt::Display for CargoStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Fetch => "cargo fetch",
            Self::Metadata => "cargo metadata",
        })
    }
}

/// Why no package set was produced. Every variant fails closed: the side has no set.
#[derive(Debug, Error)]
pub enum CargoError<E> {
    /// The copy is not an absolute UTF-8 path, so cargo cannot be pointed at it exactly.
    #[error("the copy {} is not an absolute UTF-8 path", path.display())]
    CopyPath {
        /// The refused path.
        path: PathBuf,
    },
    /// The supervisor could not run cargo, or could not show its process group gone.
    #[error("{step} could not be run")]
    Supervisor {
        /// The run that failed.
        step: CargoStep,
        /// Why.
        #[source]
        source: E,
    },
    /// cargo ran and did not succeed: a non-zero exit (an unresolvable manifest, a stale
    /// lockfile, a missing package), a signal, its deadline or its output cap.
    #[error("{step} did not succeed: {termination:?}")]
    Failed {
        /// The run that failed.
        step: CargoStep,
        /// How it ended.
        termination: Termination,
    },
    /// `cargo metadata` printed something other than UTF-8.
    #[error("cargo metadata printed output that is not UTF-8")]
    NotUtf8,
}

/// The pinned base cargo, generic over the supervisor that bounds each run.
#[derive(Debug)]
pub struct PinnedCargo<S> {
    supervisor: S,
    settings: CargoSettings,
}

impl<S> PinnedCargo<S> {
    /// Runs cargo through `supervisor`. Pass `&mut supervisor` to keep it.
    pub const fn new(supervisor: S, settings: CargoSettings) -> Self {
        Self {
            supervisor,
            settings,
        }
    }
}

impl<S: CaptureSupervisor> PinnedCargo<S> {
    /// The `cargo metadata --format-version 1 --all-features` output for the manifest-only
    /// copy at `copy` (its root `Cargo.toml`), after `cargo fetch --locked` has put every
    /// locked package in `CARGO_HOME`. Both run with `copy` as their working directory.
    ///
    /// # Errors
    ///
    /// [`CargoError::CopyPath`] before anything runs when `copy` is relative or not UTF-8;
    /// [`CargoError::Supervisor`] or [`CargoError::Failed`] when either run fails (metadata
    /// never runs after a failed fetch); [`CargoError::NotUtf8`] for unreadable output.
    pub fn metadata(&mut self, copy: &Path) -> Result<String, CargoError<S::Error>> {
        let manifest = copy
            .join("Cargo.toml")
            .into_os_string()
            .into_string()
            .ok()
            .filter(|_| copy.is_absolute())
            .ok_or_else(|| CargoError::CopyPath {
                path: copy.to_path_buf(),
            })?;
        let locate = ["--manifest-path", manifest.as_str()];
        self.run(
            copy,
            CargoStep::Fetch,
            &[&["fetch", "--locked"], &locate[..], &CONFIG[..]],
        )?;
        let metadata = [
            "metadata",
            "--locked",
            "--offline",
            "--format-version",
            "1",
            "--all-features",
        ];
        let run = self.run(
            copy,
            CargoStep::Metadata,
            &[&metadata[..], &locate[..], &CONFIG[..]],
        )?;
        String::from_utf8(run.stdout).map_err(|_utf8| CargoError::NotUtf8)
    }

    /// Runs cargo with `parts` as its arguments, in `copy`; anything but exit 0 is an error.
    fn run(
        &mut self,
        copy: &Path,
        step: CargoStep,
        parts: &[&[&str]],
    ) -> Result<CapturedRun, CargoError<S::Error>> {
        let args: Vec<String> = parts
            .iter()
            .flat_map(|part| part.iter().map(|arg| (*arg).to_owned()))
            .collect();
        let env = self.environment();
        let settings = &self.settings;
        let spec = ProcessSpec {
            program: &settings.toolchain.cargo,
            args: &args,
            cwd: copy,
            env: &env,
            timeout: match step {
                CargoStep::Fetch => settings.limits.fetch_timeout,
                CargoStep::Metadata => settings.limits.metadata_timeout,
            },
            max_output_bytes: settings.limits.max_output_bytes,
        };
        let run = self
            .supervisor
            .capture(&spec)
            .map_err(|source| CargoError::Supervisor { step, source })?;
        match run.termination {
            Termination::Exited(0) => Ok(run),
            termination => Err(CargoError::Failed { step, termination }),
        }
    }

    /// The inherited allowlist, then the variables that pin the toolchain and `CARGO_HOME`.
    fn environment(&self) -> Vec<(OsString, OsString)> {
        let settings = &self.settings;
        let pinned = [
            ("CARGO_HOME", settings.cargo_home.as_os_str()),
            ("RUSTUP_TOOLCHAIN", OsStr::new(&settings.toolchain.name)),
            ("RUSTC", settings.toolchain.rustc.as_os_str()),
            ("CARGO_TERM_COLOR", OsStr::new("never")),
        ];
        settings
            .environment
            .vars()
            .iter()
            .cloned()
            .chain(
                pinned
                    .into_iter()
                    .map(|(name, value)| (OsString::from(name), value.to_os_string())),
            )
            .collect()
    }
}
