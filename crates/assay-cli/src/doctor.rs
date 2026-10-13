//! `assay doctor <plugins-toml> <claude-program>`: the session checks of 03 §8 (#121) against
//! the pinned `claude`. One canary call in a throwaway checkout is judged for a clean init,
//! drift between the observed built-ins and `claude_cli_builtins`, a loaded mod and a followed
//! AGENTS.md/CLAUDE.md instruction.
//!
//! When `ASSAY_TOKEN_EXPIRES_ON` records the OAuth token's expiry (`YYYY-MM-DD`, 05 §5, #93),
//! a last line warns from 30 days before it (`warn token-expiry`) and fails from the date on
//! (`FAIL token-expiry`), linking the rotation runbook.
//!
//! The binary prints one line per check and exits 0 when all pass, 1 when any fails, and 2 when
//! the checks could not be run (a usage error, an unreadable or unpinned config, no token, an
//! unreadable expiry, a CLI that could not be started).

use std::ffi::{OsStr, OsString};
use std::fmt::{self, Write as _};
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use assay_app::{
    ArtifactNonce, CalendarDate, CalendarDateError, ConfigError, DoctorError, DoctorReport,
    ExpiryWarning, NonceSource, PluginsConfig, doctor, token_expiry_warning,
};
use assay_cli::ForgeSupervisor;
use assay_forge::process::{GroupKiller, KillerError};
use assay_runner::{CanaryError, ClaudeCanary, EnvVarError, HeadlessEnv};

/// The variable carrying the OAuth token, read by `main` (ARC-11).
pub(crate) const OAUTH_TOKEN: &str = "CLAUDE_CODE_OAUTH_TOKEN";

/// The variable recording the token's expiry, read by `main` (ARC-11). Not a secret: the
/// `assay-review` environment holds it next to the token (`docs/operations/token-rotation.md`).
pub(crate) const TOKEN_EXPIRES_ON: &str = "ASSAY_TOKEN_EXPIRES_ON";

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
    /// `ASSAY_TOKEN_EXPIRES_ON`; unset or empty when no expiry is recorded.
    pub(crate) expires_on: Option<OsString>,
    /// The wall clock, read once by `main`.
    pub(crate) now: SystemTime,
}

/// What `doctor` found: the session checks and the token's expiry.
pub(crate) struct DoctorOutcome {
    /// The session checks.
    pub(crate) report: DoctorReport,
    /// The expiry warning, when an expiry is recorded and near or past.
    pub(crate) expiry: Option<ExpiryWarning>,
}

impl DoctorOutcome {
    /// Whether every check passed and the token is not past its recorded expiry.
    pub(crate) fn passed(&self) -> bool {
        self.report.passed() && !expiry_fails(self.expiry.as_ref())
    }
}

/// Whether the expiry judgement fails the run: only an expired token does; a near one warns.
fn expiry_fails(expiry: Option<&ExpiryWarning>) -> bool {
    expiry.is_some_and(ExpiryWarning::is_expired)
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
    /// The recorded expiry is not a date; the value is not kept.
    ExpiresOn(CalendarDateError),
    /// The clock is outside the dates the expiry can be compared with.
    Clock(CalendarDateError),
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
            Self::ExpiresOn(source) => write!(f, "{TOKEN_EXPIRES_ON}: {source}"),
            Self::Clock(source) => write!(f, "the system clock: {source}"),
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
pub(crate) fn write_chain(
    f: &mut fmt::Formatter<'_>,
    error: &dyn std::error::Error,
) -> fmt::Result {
    write!(f, "{error}")?;
    let mut cause = error.source();
    while let Some(next) = cause {
        write!(f, ": {next}")?;
        cause = next.source();
    }
    Ok(())
}

impl std::error::Error for DoctorCliError {}

/// Reads the config and the recorded expiry, and runs the checks against `program`.
pub(crate) fn run(
    mut args: impl Iterator<Item = OsString>,
    variables: DoctorVariables,
) -> Result<DoctorOutcome, DoctorCliError> {
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
    let expiry = judge_expiry(variables.expires_on.as_deref(), variables.now)?;
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
    let report = doctor(&canary, &mut Urandom, pinned, &assay.claude_cli_builtins)
        .map_err(DoctorCliError::Doctor)?;
    Ok(DoctorOutcome { report, expiry })
}

/// The warning for the recorded expiry `expires_on` on the day of `now`; `None` when no
/// expiry is recorded (unset or empty) or it is more than 30 days away.
fn judge_expiry(
    expires_on: Option<&OsStr>,
    now: SystemTime,
) -> Result<Option<ExpiryWarning>, DoctorCliError> {
    let Some(expires_on) = expires_on.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    // A value that is not Unicode is no `YYYY-MM-DD` either.
    let expires_on = expires_on
        .to_str()
        .ok_or(CalendarDateError::Shape)
        .and_then(str::parse::<CalendarDate>)
        .map_err(DoctorCliError::ExpiresOn)?;
    let today = CalendarDate::from_system_time(now).map_err(DoctorCliError::Clock)?;
    Ok(token_expiry_warning(expires_on, today))
}

/// Writes one line per check, `ok <check>` or `FAIL <check>: <why>`, then the expiry line.
pub(crate) fn write_report(outcome: &DoctorOutcome, mut out: impl Write) -> io::Result<()> {
    for result in outcome.report.results() {
        match &result.failure {
            None => writeln!(out, "ok {}", result.check)?,
            Some(failure) => writeln!(out, "FAIL {}: {failure}", result.check)?,
        }
    }
    if let Some(warning) = &outcome.expiry {
        write_expiry(warning, &mut out)?;
    }
    out.flush()
}

/// `warn token-expiry: <why>` before the expiry, `FAIL token-expiry: <why>` from it on.
fn write_expiry(warning: &ExpiryWarning, mut out: impl Write) -> io::Result<()> {
    let level = if warning.is_expired() { "FAIL" } else { "warn" };
    writeln!(out, "{level} token-expiry: {warning}")
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// 2026-10-13, 20,739 days after the epoch.
    fn on_2026_10_13() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_hours(20_739 * 24)
    }

    #[test]
    fn an_expiry_a_day_out_is_a_warning() -> Result<(), Box<dyn std::error::Error>> {
        let judged = judge_expiry(Some(OsStr::new("2026-10-14")), on_2026_10_13())?;

        assert_eq!(
            judged,
            Some(ExpiryWarning::ExpiresSoon {
                expires_on: "2026-10-14".parse()?,
                days_left: 1,
            })
        );
        Ok(())
    }

    #[test]
    fn a_clock_before_the_epoch_is_refused() {
        let before = SystemTime::UNIX_EPOCH - Duration::from_secs(1);

        let judged = judge_expiry(Some(OsStr::new("2026-10-14")), before);

        assert!(
            matches!(
                judged,
                Err(DoctorCliError::Clock(CalendarDateError::OutOfRange))
            ),
            "{judged:?}"
        );
    }

    #[test]
    fn only_an_expired_token_fails_the_run() -> Result<(), Box<dyn std::error::Error>> {
        let soon = ExpiryWarning::ExpiresSoon {
            expires_on: "2026-11-12".parse()?,
            days_left: 30,
        };
        let expired = ExpiryWarning::Expired {
            expires_on: "2026-10-13".parse()?,
            days_ago: 0,
        };

        assert_eq!(
            [
                expiry_fails(None),
                expiry_fails(Some(&soon)),
                expiry_fails(Some(&expired))
            ],
            [false, false, true]
        );
        Ok(())
    }

    #[test]
    fn a_token_near_its_expiry_is_a_warning_line() -> Result<(), Box<dyn std::error::Error>> {
        let warning = ExpiryWarning::ExpiresSoon {
            expires_on: "2026-11-12".parse()?,
            days_left: 30,
        };
        let mut out = Vec::new();

        write_expiry(&warning, &mut out)?;

        assert_eq!(
            String::from_utf8(out)?,
            format!(
                "warn token-expiry: the OAuth token expires on 2026-11-12, in 30 days; \
                 rotate it: {}\n",
                assay_app::TOKEN_ROTATION_RUNBOOK
            )
        );
        Ok(())
    }
}
