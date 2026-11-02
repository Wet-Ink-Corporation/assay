//! `assay doctor --profile <plugins-toml> <claude-program>`: the profile checks of 03 §8 (#84)
//! against the pinned `claude`. The pin's minimum, the preflight (`ClaudeHeadless` in an empty
//! temporary directory), and one profile canary call (`ClaudeCanary`) judged for each flag, the
//! hook canary, the fence canary and the `--add-dir` boundary.
//!
//! The binary prints one line per check, `ok <check>` or `FAIL <check>: <cause chain>`, then one
//! per flag, `ok flag <flag>: <evidence>` or `FAIL flag <flag>: <check> failed`, and exits 0
//! when every check passes, 1 when one fails, and 2 when the checks could not be run.

use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};
use std::path::Path;

use assay_app::{
    EgressToken, EgressTokenError, ProfileContext, ProfileError, ProfilePorts, ProfileReport,
    verify_profile,
};
use assay_runner::{
    CanaryError, ClaudeCanary, ClaudeHeadless, ParentDeath, StatusError, Workdir, WorkdirError,
};

use super::{
    DoctorCliError, OAUTH_TOKEN, Urandom, headless_env, program_and_token, read_pins,
    system_supervisor, write_chain,
};

/// The first argument that selects this form.
pub(crate) const PROFILE_FLAG: &str = "--profile";

/// The environment the profile form reads, gathered by `main`.
pub(crate) struct ProfileVariables {
    /// `CLAUDE_CODE_OAUTH_TOKEN`.
    pub(crate) token: Option<OsString>,
    /// `PATH`, passed on to the CLI when set.
    pub(crate) path: Option<OsString>,
    /// The name of every variable of this process, for the preflight's provider check.
    pub(crate) names: Vec<OsString>,
}

/// What the profile form found.
pub(crate) struct ProfileOutcome {
    /// The profile checks.
    pub(crate) report: ProfileReport<StatusError>,
    /// Why the preflight's empty directory could not be removed, if it could not.
    pub(crate) left: Option<io::Error>,
}

/// Why the profile checks could not be run.
#[derive(Debug)]
pub(crate) enum ProfileCliError {
    /// Not exactly two arguments after `--profile`.
    Usage,
    /// A refusal shared with the session form: the program, the token, the config, the killer.
    Setup(DoctorCliError),
    /// The token cannot be scanned for in the transcript; the value is not kept.
    TokenScan(EgressTokenError),
    /// The preflight's empty working directory could not be made or removed.
    Scratch(io::Error),
    /// The preflight's working directory is unusable.
    Workdir(WorkdirError),
    /// The canary could not be run, or no nonce could be drawn; the preflight's empty directory
    /// is still removed, and `left` says why it could not be.
    Profile {
        /// Why the checks could not be run.
        error: ProfileError<CanaryError, io::Error>,
        /// Why the preflight's empty directory could not be removed, if it could not.
        left: Option<io::Error>,
    },
}

impl fmt::Display for ProfileCliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage => write!(
                f,
                "usage: assay doctor {PROFILE_FLAG} <plugins-toml> <claude-program>"
            ),
            Self::Setup(source) => write!(f, "{source}"),
            Self::TokenScan(source) => write!(f, "{OAUTH_TOKEN} cannot be scanned for: {source}"),
            Self::Scratch(source) => {
                write!(f, "the preflight's empty directory: {source}")
            }
            Self::Workdir(source) => write_chain(f, source),
            Self::Profile { error, left } => {
                write_chain(f, error)?;
                match left {
                    Some(left) => write!(f, "; and the preflight's empty directory: {left}"),
                    None => Ok(()),
                }
            }
        }
    }
}

impl std::error::Error for ProfileCliError {}

/// Reads the config and runs the profile checks against `program`.
pub(crate) fn run(
    mut args: impl Iterator<Item = OsString>,
    variables: ProfileVariables,
) -> Result<ProfileOutcome, ProfileCliError> {
    let (Some(config), Some(program), None) = (args.next(), args.next(), args.next()) else {
        return Err(ProfileCliError::Usage);
    };
    let (program, token) =
        program_and_token(program, variables.token).map_err(ProfileCliError::Setup)?;
    let scanned = EgressToken::new(&token).map_err(ProfileCliError::TokenScan)?;
    let config = read_pins(Path::new(&config)).map_err(ProfileCliError::Setup)?;
    let assay = config.assay();
    let pinned = assay
        .claude_cli
        .as_ref()
        .ok_or(ProfileCliError::Setup(DoctorCliError::NoPin))?;
    let env = headless_env(token, variables.path).map_err(ProfileCliError::Setup)?;
    let supervisor = system_supervisor().map_err(ProfileCliError::Setup)?;
    let empty = tempfile::tempdir().map_err(ProfileCliError::Scratch)?;
    let workdir = Workdir::new(empty.path()).map_err(ProfileCliError::Workdir)?;
    // Each adapter keeps its own program path, environment and parent-death guard: the same
    // values, held twice.
    let parent_death = ParentDeath::probe();
    let cli = ClaudeHeadless::new(program.clone(), workdir, env.clone(), &supervisor)
        .with_parent_death(parent_death.clone());
    let canary = ClaudeCanary::new(program, env, &supervisor).with_parent_death(parent_death);
    let ports = ProfilePorts {
        cli: &cli,
        probe: &canary,
        nonces: &mut Urandom,
    };
    let context = ProfileContext {
        pinned,
        allowlist: &assay.claude_cli_builtins,
        token: &scanned,
    };
    let verdict = verify_profile(variables.names, ports, &context);
    // The directory is removed whether or not the checks ran; a failure to remove it is reported
    // beside the outcome, never instead of it.
    let left = empty.close().err();
    match verdict {
        Ok(report) => Ok(ProfileOutcome { report, left }),
        Err(error) => Err(ProfileCliError::Profile { error, left }),
    }
}

/// Writes one line per check, then one per flag.
pub(crate) fn write_report(
    report: &ProfileReport<StatusError>,
    mut out: impl Write,
) -> io::Result<()> {
    for result in report.results() {
        match &result.failure {
            None => writeln!(out, "ok {}", result.check)?,
            Some(failure) => writeln!(out, "FAIL {}: {}", result.check, Chain(failure))?,
        }
    }
    for flag in report.flags() {
        let level = if flag.failed.is_none() { "ok" } else { "FAIL" };
        writeln!(out, "{level} flag {flag}")?;
    }
    out.flush()
}

/// An error and each of its causes, joined by `: `.
struct Chain<'e>(&'e dyn std::error::Error);

impl fmt::Display for Chain<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_chain(f, self.0)
    }
}
