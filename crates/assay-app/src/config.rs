//! Base configuration: `.assay/plugins.toml`, read from `base_sha`
//! (`docs/architecture/03-contracts.md` §1).
//!
//! The text is parsed in-process into a DTO that rejects unknown keys (so there is no
//! `applies_when`: required gates always run), then validated into the types below. Pins and
//! digests accept immutable identities only; a tag is [`ParsePinError::Tag`].

mod assay;
mod builtins;
mod dto;
mod error;
mod globs;
mod identities;
mod name;
mod owners;
mod pin;
mod reserved;

use std::collections::BTreeSet;
use std::str::FromStr;
use std::time::Duration;

pub use self::assay::{AssaySettings, ExactVersion, RunnerKind};
pub use self::builtins::{
    BuiltinAllowlist, BuiltinKind, BuiltinsError, MAX_BUILTIN_NAME_BYTES, MAX_BUILTINS,
};
use self::dto::{AssayDto, FileDto, PluginDto, StepDto};
pub use self::error::{ConfigError, TomlError};
pub use self::globs::{GlobsConfigError, parse_globs};
pub use self::identities::IdentitiesConfigError;
pub(crate) use self::identities::{parse_agents, parse_owners};
pub use self::name::GateName;
pub use self::owners::{
    EligibilityLists, Identity, ListFile, ListName, ListsError, MAX_LIST_ENTRIES, MAX_LISTS_BYTES,
};
pub use self::pin::{
    CommitId, ParseCommitIdError, ParseDigestError, ParsePinError, Pin, Sha256Digest,
};
use self::reserved::Builtin;
pub(crate) use self::reserved::WEIGH_IN_NAME;
use crate::gate::BuildJob;

/// A gate's timeout when `timeout_s` is omitted, in seconds.
pub const DEFAULT_TIMEOUT_SECS: u64 = 1_200;

/// The longest accepted `timeout_s`: six hours, the most a GitHub-hosted job can run.
pub const MAX_TIMEOUT_SECS: u64 = 21_600;

/// The validated contents of `.assay/plugins.toml`. Parse it with [`str::parse`].
///
/// # Examples
///
/// ```
/// use assay_app::{Adapter, PluginsConfig};
///
/// let config: PluginsConfig = r#"
/// [assay]
/// digest = "sha256:dc7c1d8e3ccab68cfdb828ce40a446c3913f7172f899ffe3f439839cfd917a8b"
///
/// [[plugin]]
/// name = "custom-lint"
/// adapter = "exec"
/// exec = ["./scripts/lint.sh", "--json"]
/// job = "lint"
/// required = false
/// "#
/// .parse()?;
///
/// let [plugin] = config.plugins() else {
///     return Err("one plugin expected".into());
/// };
/// assert!(matches!(plugin.adapter(), Adapter::Exec { .. }));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginsConfig {
    assay: AssaySettings,
    binary_globs: Vec<String>,
    plugins: Vec<Plugin>,
}

impl PluginsConfig {
    /// The `[assay]` table.
    #[must_use]
    pub const fn assay(&self) -> &AssaySettings {
        &self.assay
    }

    /// `[gates] binary_globs`: paths whose git attributes may be set (`03 §2.1`); empty when
    /// the file names none.
    #[must_use]
    pub fn binary_globs(&self) -> &[String] {
        &self.binary_globs
    }

    /// The `[[plugin]]` entries, in file order, with distinct names.
    #[must_use]
    pub fn plugins(&self) -> &[Plugin] {
        &self.plugins
    }
}

impl FromStr for PluginsConfig {
    type Err = ConfigError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let file: FileDto = toml::from_str(text).map_err(|e| ConfigError::Syntax(TomlError(e)))?;
        let plugins = file
            .plugins
            .into_iter()
            .map(Plugin::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(name) = first_duplicate(plugins.iter().map(Plugin::name)) {
            return Err(ConfigError::DuplicateName { name });
        }
        Ok(Self {
            assay: AssaySettings::try_from(file.assay)?,
            binary_globs: file.gates.binary_globs,
            plugins,
        })
    }
}

/// The first name that occurs twice, as text.
fn first_duplicate<'a>(names: impl IntoIterator<Item = &'a GateName>) -> Option<String> {
    let mut seen = BTreeSet::new();
    names
        .into_iter()
        .find(|name| !seen.insert(*name))
        .map(|name| name.as_str().to_owned())
}

/// One `[[plugin]]` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plugin {
    name: GateName,
    adapter: Adapter,
    required: bool,
    timeout: Duration,
}

impl Plugin {
    /// The plugin's name, unique in the file.
    #[must_use]
    pub const fn name(&self) -> &GateName {
        &self.name
    }

    /// The adapter that runs it, with that adapter's settings.
    #[must_use]
    pub const fn adapter(&self) -> &Adapter {
        &self.adapter
    }

    /// Whether the gate is required. A required gate always runs; one that does not report is
    /// `gate-error:skipped`, never a pass.
    #[must_use]
    pub const fn required(&self) -> bool {
        self.required
    }

    /// The wall-clock limit of the adapter's own run ([`DEFAULT_TIMEOUT_SECS`] when omitted).
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }
}

/// Which adapter runs a plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Adapter {
    /// `builtin:temper`, with its declared base steps.
    Temper {
        /// The temper release to run.
        pin: Pin,
        /// `[[plugin.step]]` entries, in file order, with distinct names.
        steps: Vec<Step>,
    },
    /// `builtin:weigh-in`.
    WeighIn {
        /// The Weigh-In release to run.
        pin: Pin,
    },
    /// `exec`: a PR-controlled command, run only in a Z2b job.
    Exec {
        /// The program and its arguments.
        command: Argv,
        /// The Z2b job it runs in.
        job: BuildJob,
    },
}

/// One `[[plugin.step]]`: a declared base step run in its Z2b job under its own timeout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    name: GateName,
    job: BuildJob,
    command: Argv,
    timeout: Duration,
}

impl Step {
    /// The step's name, unique within its plugin.
    #[must_use]
    pub const fn name(&self) -> &GateName {
        &self.name
    }

    /// The Z2b job it runs in.
    #[must_use]
    pub const fn job(&self) -> BuildJob {
        self.job
    }

    /// The program and its arguments.
    #[must_use]
    pub const fn command(&self) -> &Argv {
        &self.command
    }

    /// Its wall-clock limit.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }
}

/// A command line: a non-empty program and its arguments, none empty or holding a NUL byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Argv {
    program: String,
    args: Vec<String>,
}

impl Argv {
    fn new(argv: Vec<String>) -> Option<Self> {
        let valid = argv
            .iter()
            .all(|arg| !arg.is_empty() && !arg.contains('\0'));
        let mut argv = argv.into_iter();
        let program = argv.next()?;
        valid.then(|| Self {
            program,
            args: argv.collect(),
        })
    }

    /// The program to run.
    #[must_use]
    pub fn program(&self) -> &str {
        &self.program
    }

    /// The arguments after the program.
    #[must_use]
    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// The program, then each argument.
    pub fn iter(&self) -> impl Iterator<Item = &str> + '_ {
        std::iter::once(self.program.as_str()).chain(self.args.iter().map(String::as_str))
    }
}

impl TryFrom<AssayDto> for AssaySettings {
    type Error = ConfigError;

    fn try_from(dto: AssayDto) -> Result<Self, Self::Error> {
        let digest = |field, text: &str| {
            text.parse()
                .map_err(|source| ConfigError::Digest { field, source })
        };
        let claude_cli = dto
            .claude_cli
            .map(|text| ExactVersion::new(text).map_err(|version| ConfigError::Version { version }))
            .transpose()?;
        let claude_cli_builtins = match (dto.claude_cli_builtins, &claude_cli) {
            (None, _) => BuiltinAllowlist::default(),
            (Some(_), None) => return Err(BuiltinsError::WithoutCli.into()),
            (Some(builtins), Some(_)) => BuiltinAllowlist::try_from(builtins)?,
        };
        Ok(Self {
            digest: digest("assay.digest", &dto.digest)?,
            warm_target: dto
                .warm_target
                .as_deref()
                .map(|text| digest("assay.warm_target", text))
                .transpose()?,
            claude_cli,
            claude_cli_builtins,
            runner: dto.runner,
            protected_bases: dto.protected_bases,
            llm_blockers: dto.llm_blockers,
            llm_majors: dto.llm_majors,
            on_infra_failure: dto.on_infra_failure,
            block_on_infra_for: dto.block_on_infra_for,
            ledger_repo: dto.ledger_repo,
        })
    }
}

impl TryFrom<PluginDto> for Plugin {
    type Error = ConfigError;

    fn try_from(dto: PluginDto) -> Result<Self, Self::Error> {
        let name = GateName::new(dto.name).map_err(|name| ConfigError::Name { name })?;
        let gate = name.as_str();
        Builtin::check_name(gate, &dto.adapter)?;
        let timeout = timeout(gate, dto.timeout_s.unwrap_or(DEFAULT_TIMEOUT_SECS))?;
        let adapter = match Builtin::from_adapter(&dto.adapter) {
            Some(Builtin::Temper) => {
                refuse(gate, "exec", dto.exec.is_some())?;
                refuse(gate, "job", dto.job.is_some())?;
                let steps = dto
                    .steps
                    .unwrap_or_default()
                    .into_iter()
                    .map(|step| Step::from_dto(gate, step))
                    .collect::<Result<Vec<_>, _>>()?;
                if let Some(step) = first_duplicate(steps.iter().map(Step::name)) {
                    return Err(ConfigError::DuplicateName {
                        name: format!("{gate}.{step}"),
                    });
                }
                Adapter::Temper {
                    pin: pin(gate, dto.pin)?,
                    steps,
                }
            }
            Some(Builtin::WeighIn) => {
                refuse(gate, "exec", dto.exec.is_some())?;
                refuse(gate, "job", dto.job.is_some())?;
                refuse(gate, "step", dto.steps.is_some())?;
                Adapter::WeighIn {
                    pin: pin(gate, dto.pin)?,
                }
            }
            None if dto.adapter == "exec" => {
                refuse(gate, "pin", dto.pin.is_some())?;
                refuse(gate, "step", dto.steps.is_some())?;
                let argv = dto.exec.ok_or_else(|| missing(gate, "exec"))?;
                let job = dto.job.ok_or_else(|| missing(gate, "job"))?;
                Adapter::Exec {
                    command: argv_of(gate, argv)?,
                    job: job_of(gate, job)?,
                }
            }
            None => {
                return Err(ConfigError::UnknownAdapter {
                    gate: gate.to_owned(),
                    adapter: dto.adapter,
                });
            }
        };
        Ok(Self {
            name,
            adapter,
            required: dto.required,
            timeout,
        })
    }
}

impl Step {
    fn from_dto(plugin: &str, dto: StepDto) -> Result<Self, ConfigError> {
        let name = GateName::new(dto.name).map_err(|name| ConfigError::Name { name })?;
        let gate = format!("{plugin}.{name}");
        let job = job_of(&gate, dto.job)?;
        let command = argv_of(&gate, dto.cmd)?;
        if let Some(arg) = command.args().iter().find(|arg| lets_zero_tests_pass(arg)) {
            return Err(ConfigError::NoTests {
                // The error owns its copy of the refused argument; the command is dropped.
                arg: arg.clone(),
                gate,
            });
        }
        if job == BuildJob::Test && runs_nextest(&command) && !fails_zero_tests(&command) {
            return Err(ConfigError::NoTestsFailMissing { gate });
        }
        Ok(Self {
            job,
            command,
            timeout: timeout(&gate, dto.timeout_s)?,
            name,
        })
    }
}

/// Whether `arg` sets nextest's `--no-tests` to anything but exactly `fail`: `--no-tests=pass`,
/// `=warn`, an empty or differently cased value, or the bare flag, whose value is the next
/// argument. Refused rather than modelled, so a declared step can never let zero tests pass.
fn lets_zero_tests_pass(arg: &str) -> bool {
    arg != "--no-tests=fail" && (arg == "--no-tests" || arg.starts_with("--no-tests="))
}

/// The arguments nextest reads as its own: those before the first `--`, after which they go to
/// the test binaries.
fn own_args(command: &Argv) -> impl Iterator<Item = &str> + '_ {
    command
        .args()
        .iter()
        .map(String::as_str)
        .take_while(|arg| *arg != "--")
}

/// Whether `command` runs nextest: its program's file name is `cargo-nextest`, or one of its
/// own arguments is the `nextest` subcommand. Read wide rather than modelled (any `nextest`
/// argument, wherever cargo's options put it), so a nextest run cannot slip past
/// [`fails_zero_tests`].
fn runs_nextest(command: &Argv) -> bool {
    let program = command.program();
    let file_name = program.rsplit('/').next().unwrap_or(program);
    file_name == "cargo-nextest" || own_args(command).any(|arg| arg == "nextest")
}

/// Whether nextest reads `--no-tests=fail` as its own argument.
fn fails_zero_tests(command: &Argv) -> bool {
    own_args(command).any(|arg| arg == "--no-tests=fail")
}

fn pin(gate: &str, pin: Option<String>) -> Result<Pin, ConfigError> {
    pin.ok_or_else(|| missing(gate, "pin"))?
        .parse()
        .map_err(|source| ConfigError::Pin {
            plugin: gate.to_owned(),
            source,
        })
}

fn timeout(gate: &str, seconds: u64) -> Result<Duration, ConfigError> {
    if (1..=MAX_TIMEOUT_SECS).contains(&seconds) {
        Ok(Duration::from_secs(seconds))
    } else {
        Err(ConfigError::Timeout {
            gate: gate.to_owned(),
            seconds,
        })
    }
}

fn argv_of(gate: &str, argv: Vec<String>) -> Result<Argv, ConfigError> {
    Argv::new(argv).ok_or_else(|| ConfigError::Command {
        gate: gate.to_owned(),
    })
}

fn job_of(gate: &str, job: String) -> Result<BuildJob, ConfigError> {
    BuildJob::from_config(&job).ok_or_else(|| ConfigError::Job {
        gate: gate.to_owned(),
        job,
    })
}

fn refuse(gate: &str, key: &'static str, present: bool) -> Result<(), ConfigError> {
    if present {
        Err(ConfigError::UnexpectedKey {
            gate: gate.to_owned(),
            key,
        })
    } else {
        Ok(())
    }
}

fn missing(gate: &str, key: &'static str) -> ConfigError {
    ConfigError::MissingKey {
        gate: gate.to_owned(),
        key,
    }
}
