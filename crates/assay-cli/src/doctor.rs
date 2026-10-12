//! `assay doctor <plugins-toml> <claude-program>`: the session checks of 03 §8 (#121) against
//! the pinned `claude`. One canary call in a throwaway checkout is judged for a clean init,
//! drift between the observed built-ins and `claude_cli_builtins`, a loaded mod and a followed
//! AGENTS.md/CLAUDE.md instruction.
//!
//! The binary prints one line per check and exits 0 when all pass, 1 when any fails, and 2 when
//! the checks could not be run (a usage error, an unreadable or unpinned config, no token, a CLI
//! that could not be started).

use std::ffi::OsString;
use std::fmt::{self, Write as _};
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use assay_app::{
    ArtifactNonce, ConfigError, DoctorError, DoctorReport, NonceSource, PluginsConfig, doctor,
};
use assay_cli::ForgeSupervisor;
use assay_forge::process::{GroupKiller, KillerError};
use assay_runner::{CanaryError, ClaudeCanary, EnvVarError, HeadlessEnv};

/// The variable carrying the OAuth token, read by `main` (ARC-11).
pub(crate) const OAUTH_TOKEN: &str = "CLAUDE_CODE_OAUTH_TOKEN";

/// Largest `plugins.toml` read, in bytes: 1 MiB (H-13).
const MAX_CONFIG_BYTES: u64 = 1 << 20;

/// Bytes of entropy in a canary nonce: 128 bits, 32 hex digits.
const NONCE_BYTES: usize = 16;

/// Where nonces come from.
const URANDOM: &str = "/dev/urandom";

/// The environment `doctor` reads, gathered by `main`.
pub(crate) struct DoctorVariables {
    /// `CLAUDE_CODE_OAUTH_TOKEN`.
    pub(crate) token: Option<OsString>,
    /// `PATH`, passed on to the CLI when set.
    pub(crate) path: Option<OsString>,
}

/// Why the checks could not be run.
#[derive(Debug)]
pub(crate) enum DoctorCliError {
    /// Not exactly two arguments.
    Usage,
    /// The program is a relative path, which would be looked up rather than named.
    RelativeProgram(PathBuf),
    /// The token is missing or empty.
    Token,
    /// The token is not valid Unicode.
    TokenNotUnicode,
    /// The config could not be read.
    Read { path: PathBuf, source: io::Error },
    /// The config is over [`MAX_CONFIG_BYTES`].
    TooLarge { path: PathBuf },
    /// The config is not UTF-8.
    NotUtf8 { path: PathBuf },
    /// The config was refused.
    Config(ConfigError),
    /// The config pins no `claude_cli`.
    NoPin,
    /// No process-group killer is available on this host.
    Killer(KillerError),
    /// `PATH` could not be passed to the CLI.
    ///
    /// `HeadlessEnv::pass` checks every name it is given, so its `Result` must be handled.
    /// `PATH` is a plain identifier that is not reserved, so this does not occur today; it
    /// would after a change to the runner's reserved names, and is then reported, not lost.
    Env(EnvVarError),
    /// The canary could not be run, or no nonce could be drawn.
    Doctor(DoctorError<CanaryError, io::Error>),
}

impl fmt::Display for DoctorCliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage => f.write_str("usage: assay doctor <plugins-toml> <claude-program>"),
            Self::RelativeProgram(path) => write!(
                f,
                "the claude program {} is not an absolute path",
                path.display()
            ),
            Self::Token => write!(f, "{OAUTH_TOKEN} is not set"),
            Self::TokenNotUnicode => write!(f, "{OAUTH_TOKEN} is not valid Unicode"),
            Self::Read { path, source } => write!(f, "cannot read {}: {source}", path.display()),
            Self::TooLarge { path } => write!(
                f,
                "{} is larger than {MAX_CONFIG_BYTES} bytes",
                path.display()
            ),
            Self::NotUtf8 { path } => write!(f, "{} is not UTF-8", path.display()),
            Self::Config(source) => write_chain(f, source),
            Self::NoPin => {
                f.write_str("plugins.toml pins no claude_cli, so there is no CLI to check")
            }
            Self::Killer(source) => {
                f.write_str("no process-group killer: ")?;
                write_chain(f, source)
            }
            Self::Env(source) => write_chain(f, source),
            Self::Doctor(source) => write_chain(f, source),
        }
    }
}

/// `error` and each of its causes, joined by `: `.
fn write_chain(f: &mut fmt::Formatter<'_>, error: &dyn std::error::Error) -> fmt::Result {
    write!(f, "{error}")?;
    let mut cause = error.source();
    while let Some(next) = cause {
        write!(f, ": {next}")?;
        cause = next.source();
    }
    Ok(())
}

impl std::error::Error for DoctorCliError {}

/// Reads the config and runs the checks against `program`.
pub(crate) fn run(
    mut args: impl Iterator<Item = OsString>,
    variables: DoctorVariables,
) -> Result<DoctorReport, DoctorCliError> {
    let (Some(config), Some(program), None) = (args.next(), args.next(), args.next()) else {
        return Err(DoctorCliError::Usage);
    };
    let program = PathBuf::from(program);
    if !program.is_absolute() {
        return Err(DoctorCliError::RelativeProgram(program));
    }
    let token = match variables.token {
        None => return Err(DoctorCliError::Token),
        Some(token) if token.is_empty() => return Err(DoctorCliError::Token),
        Some(token) => match token.into_string() {
            Ok(token) => token,
            // The rejected value is the secret itself; it is not kept.
            Err(_) => return Err(DoctorCliError::TokenNotUnicode),
        },
    };
    let config: PluginsConfig = read_config(Path::new(&config))?
        .parse()
        .map_err(DoctorCliError::Config)?;
    let assay = config.assay();
    let pinned = assay.claude_cli.as_ref().ok_or(DoctorCliError::NoPin)?;
    let mut env = HeadlessEnv::new(token);
    if let Some(path) = variables.path {
        env.pass("PATH", path).map_err(DoctorCliError::Env)?;
    }
    let supervisor = ForgeSupervisor::new(GroupKiller::system().map_err(DoctorCliError::Killer)?);
    let canary = ClaudeCanary::new(program, env, &supervisor);
    doctor(&canary, &mut Urandom, pinned, &assay.claude_cli_builtins)
        .map_err(DoctorCliError::Doctor)
}

/// Writes one line per check: `ok <check>` or `FAIL <check>: <why>`.
pub(crate) fn write_report(report: &DoctorReport, mut out: impl Write) -> io::Result<()> {
    for result in report.results() {
        match &result.failure {
            None => writeln!(out, "ok {}", result.check)?,
            Some(failure) => writeln!(out, "FAIL {}: {failure}", result.check)?,
        }
    }
    out.flush()
}

/// `path` as text, read through a [`MAX_CONFIG_BYTES`] limit.
fn read_config(path: &Path) -> Result<String, DoctorCliError> {
    let unreadable = |source| DoctorCliError::Read {
        path: path.to_path_buf(),
        source,
    };
    let file = File::open(path).map_err(unreadable)?;
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(unreadable)?;
    if u64::try_from(bytes.len()).map_or(true, |len| len > MAX_CONFIG_BYTES) {
        return Err(DoctorCliError::TooLarge {
            path: path.to_path_buf(),
        });
    }
    match String::from_utf8(bytes) {
        Ok(text) => Ok(text),
        Err(_) => Err(DoctorCliError::NotUtf8 {
            path: path.to_path_buf(),
        }),
    }
}

/// Nonces from the kernel's random source.
struct Urandom;

impl NonceSource for Urandom {
    type Error = io::Error;

    fn nonce(&mut self) -> Result<ArtifactNonce, io::Error> {
        let mut bytes = [0_u8; NONCE_BYTES];
        File::open(URANDOM)?.read_exact(&mut bytes)?;
        let mut hex = String::with_capacity(NONCE_BYTES.saturating_mul(2));
        for byte in bytes {
            write!(hex, "{byte:02x}").map_err(io::Error::other)?;
        }
        hex.parse().map_err(io::Error::other)
    }
}
