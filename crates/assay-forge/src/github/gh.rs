//! The production [`Transport`]: one `gh api --include` process per request (#273, option B of
//! wi-258667).
//!
//! This crate decides everything about the call (program, arguments, environment, working
//! directory, stdin, limits) and reads what gh wrote; a [`GhRunner`] only runs it. `assay-cli`
//! lends forge's own process-group supervisor ([`process::run`](crate::process::run)) as the
//! runner, so the git constructor stays the only process this crate creates
//! (`tests/it/single_constructor.rs`).

mod environment;
mod reply;

use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Duration;

use tempfile::TempDir;

use super::transport::{MAX_RESPONSE_BYTES, Request, Response, Transport};
use crate::process::{Captured, Limits, ProcessError};

pub use environment::GhEnvironment;

/// A request's default deadline, from start until gh's process group is killed.
pub const GH_TIMEOUT: Duration = Duration::from_secs(30);

/// The most bytes gh may write on stdout, and separately on stderr.
///
/// It is the response head's cap (64 KiB), [`MAX_RESPONSE_BYTES`] and one byte, so that a body
/// just over the adapter's cap still reaches the adapter, which refuses it, and a larger answer
/// stops the process.
pub const GH_OUTPUT_CAP: usize = reply::MAX_HEAD_BYTES + MAX_RESPONSE_BYTES + 1;

/// The most bytes of gh's stderr an error keeps.
const STDERR_EXCERPT: usize = 512;

/// What replaces the token wherever gh's stderr is kept.
const REDACTED: &str = "<redacted>";

/// The headers sent with every request: GitHub's JSON media type and a pinned API version.
const ACCEPT: &str = "Accept: application/vnd.github+json";
const API_VERSION: &str = "X-GitHub-Api-Version: 2022-11-28";
const CONTENT_TYPE: &str = "Content-Type: application/json";

/// The words gh fills in after a `:` (`gh api`'s `:owner`, `:repo`, `:branch`).
const COLON_PLACEHOLDERS: [&str; 3] = ["owner", "repo", "branch"];

/// The token gh authenticates with (`GH_TOKEN`): not empty, printable ASCII. Its `Debug`
/// never shows it, and it is neither cloned nor compared.
pub struct GhToken(String);

/// Why a token was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a GitHub token is not empty and is printable ASCII")]
pub struct InvalidToken;

impl GhToken {
    /// `secret` as a token.
    ///
    /// # Errors
    ///
    /// [`InvalidToken`] when it is empty or holds anything but printable ASCII.
    pub fn new(secret: impl Into<String>) -> Result<Self, InvalidToken> {
        let secret = secret.into();
        if secret.is_empty() || !secret.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err(InvalidToken);
        }
        Ok(Self(secret))
    }
}

impl fmt::Debug for GhToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GhToken(<redacted>)")
    }
}

/// One `gh api` process, as [`GhApi`] decided it.
///
/// Run `program` with exactly `args`, `env` (nothing inherited), `cwd` and `stdin`, under
/// `limits`. Its `Debug` shows the variables' names, never their values (the token, a proxy's
/// credentials).
#[derive(Clone, Copy)]
pub struct GhCall<'a> {
    /// The absolute path of gh.
    pub program: &'a Path,
    /// Its arguments.
    pub args: &'a [&'a str],
    /// Its whole environment.
    pub env: &'a [(&'a str, &'a OsStr)],
    /// Its working directory: the scratch configuration directory, never a checkout.
    pub cwd: &'a Path,
    /// What it reads on stdin: the request body, or nothing.
    pub stdin: &'a [u8],
    /// Its timeout and output cap.
    pub limits: Limits,
}

impl fmt::Debug for GhCall<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.env.iter().map(|&(name, _)| name).collect();
        f.debug_struct("GhCall")
            .field("program", &self.program)
            .field("args", &self.args)
            .field("env", &names)
            .field("cwd", &self.cwd)
            .field("stdin_bytes", &self.stdin.len())
            .field("limits", &self.limits)
            .finish()
    }
}

/// Runs a [`GhCall`] in its own process group within its limits, the group killed and shown
/// gone on a timeout or an over-cap output (H-13).
pub trait GhRunner {
    /// Runs `call` and returns how gh exited and what it wrote.
    ///
    /// # Errors
    ///
    /// [`ProcessError`] when gh could not be started or observed, ran past its timeout or
    /// wrote past its cap; [`ProcessError::Kill`] means its group may still run (abort the
    /// job).
    fn run(&self, call: &GhCall<'_>) -> Result<Captured, ProcessError>;
}

impl<R: GhRunner + ?Sized> GhRunner for &R {
    fn run(&self, call: &GhCall<'_>) -> Result<Captured, ProcessError> {
        (**self).run(call)
    }
}

/// Why a [`GhApi`] could not be built.
#[derive(Debug, thiserror::Error)]
pub enum GhSetupError {
    /// gh was not named by an absolute path; its environment has no `PATH` to search.
    #[error("gh must be named by an absolute path, not {}", .0.display())]
    RelativeProgram(PathBuf),
    /// The scratch configuration directory could not be created.
    #[error("could not create gh's scratch configuration directory")]
    ConfigDir(#[source] io::Error),
}

/// Why `gh api` gave no response.
#[derive(Debug, thiserror::Error)]
pub enum GhError {
    /// The path holds `{` or `}`, or `:owner`, `:repo` or `:branch` not followed by a letter,
    /// digit or `_`: gh would fill those in from a local checkout.
    #[error("the path holds a placeholder gh would fill in")]
    Placeholder,
    /// gh could not be run within its limits.
    #[error("gh api did not complete")]
    Run(#[source] ProcessError),
    /// gh failed before writing a response.
    // `{stderr:?}`: escaped, so gh's output cannot inject control sequences into a log.
    #[error("gh api failed ({exit}) without a response: {stderr:?}")]
    NoResponse {
        /// How gh exited.
        exit: ExitStatus,
        /// The start of what gh wrote on stderr, read lossily, the token replaced by
        /// `<redacted>`.
        stderr: String,
    },
    /// gh's output is not a response as `--include` writes it.
    #[error("gh api's output is malformed: {detail}")]
    Malformed {
        /// What is wrong with it.
        detail: &'static str,
    },
    /// gh's exit does not agree with the status it wrote: 0 goes with a 2xx, 1 with any
    /// other status (or with any status from `graphql`, on GraphQL errors).
    #[error("gh api ended with {exit} after an HTTP {status} response")]
    Inconsistent {
        /// How gh exited.
        exit: ExitStatus,
        /// The status it wrote.
        status: u16,
    },
}

/// The GitHub API through `gh api`, authenticated with one token, in a scratch configuration
/// directory this transport owns.
#[derive(Debug)]
pub struct GhApi<R> {
    runner: R,
    program: PathBuf,
    token: GhToken,
    environment: GhEnvironment,
    config: TempDir,
    timeout: Duration,
}

impl<R> GhApi<R> {
    /// gh at the absolute path `program`, run by `runner` with `token` and the variables of
    /// `environment`, in a new scratch configuration directory.
    ///
    /// Each request has [`GH_TIMEOUT`] unless [`with_timeout`](Self::with_timeout) sets
    /// another. Dropping the transport removes the directory too; [`close`](Self::close)
    /// reports a failure to.
    ///
    /// # Errors
    ///
    /// [`GhSetupError::RelativeProgram`] when `program` is not absolute,
    /// [`GhSetupError::ConfigDir`] when the directory cannot be created.
    pub fn new(
        runner: R,
        program: impl Into<PathBuf>,
        token: GhToken,
        environment: GhEnvironment,
    ) -> Result<Self, GhSetupError> {
        let program = program.into();
        if !program.is_absolute() {
            return Err(GhSetupError::RelativeProgram(program));
        }
        let config = tempfile::Builder::new()
            .prefix("assay-gh-")
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()
            .map_err(GhSetupError::ConfigDir)?;
        Ok(Self {
            runner,
            program,
            token,
            environment,
            config,
            timeout: GH_TIMEOUT,
        })
    }

    /// The same transport with `timeout` per request.
    #[must_use]
    pub fn with_timeout(self, timeout: Duration) -> Self {
        Self { timeout, ..self }
    }

    /// Removes the scratch configuration directory.
    ///
    /// # Errors
    ///
    /// The I/O error when it cannot be removed.
    pub fn close(self) -> io::Result<()> {
        self.config.close()
    }
}

impl<R: GhRunner> Transport for GhApi<R> {
    type Error = GhError;

    /// Runs `gh api --include` for `request` and reads its response.
    ///
    /// # Errors
    ///
    /// [`GhError`] when no response was received or gh's output cannot be read exactly.
    fn send(&self, request: &Request<'_>) -> Result<Response, GhError> {
        let path = request.path.as_str();
        if holds_placeholder(path) {
            return Err(GhError::Placeholder);
        }
        let mut args = vec![
            "api",
            "--include",
            "--method",
            request.method.as_str(),
            "--header",
            ACCEPT,
            "--header",
            API_VERSION,
        ];
        if request.body.is_some() {
            args.extend(["--header", CONTENT_TYPE, "--input", "-"]);
        }
        args.extend(["--", path]);
        let config = self.config.path();
        let mut env = vec![
            ("GH_TOKEN", OsStr::new(&self.token.0)),
            ("GH_HOST", OsStr::new("github.com")),
            ("GH_PROMPT_DISABLED", OsStr::new("1")),
            ("NO_COLOR", OsStr::new("1")),
            ("GH_NO_UPDATE_NOTIFIER", OsStr::new("1")),
            ("GH_CONFIG_DIR", config.as_os_str()),
        ];
        env.extend(self.environment.vars());
        let call = GhCall {
            program: &self.program,
            args: &args,
            env: &env,
            cwd: config,
            stdin: request.body.unwrap_or_default(),
            limits: Limits {
                timeout: self.timeout,
                max_output_bytes: GH_OUTPUT_CAP,
            },
        };
        let captured = self.runner.run(&call).map_err(GhError::Run)?;
        read(captured, path == "graphql", &self.token)
    }
}

/// Whether gh would fill something into `path`: `{...}` anywhere, or `:owner`, `:repo` or
/// `:branch` followed by anything but an ASCII letter, digit or `_` (Go's `\b`, as gh's
/// placeholder pattern reads it).
fn holds_placeholder(path: &str) -> bool {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    path.contains(['{', '}'])
        || path.split(':').skip(1).any(|after_colon| {
            COLON_PLACEHOLDERS.iter().any(|placeholder| {
                after_colon
                    .strip_prefix(placeholder)
                    .is_some_and(|rest| !rest.starts_with(word))
            })
        })
}

/// The start of `stderr`, read lossily, with `token` replaced and at most
/// [`STDERR_EXCERPT`] bytes kept.
fn excerpt(stderr: &[u8], token: &GhToken) -> String {
    // Enough to hold a token that starts inside the excerpt.
    let window = STDERR_EXCERPT.saturating_add(token.0.len());
    let start = stderr.get(..window).unwrap_or(stderr);
    let mut text = String::from_utf8_lossy(start).replace(&token.0, REDACTED);
    let end = (0..=STDERR_EXCERPT.min(text.len()))
        .rev()
        .find(|&at| text.is_char_boundary(at))
        .unwrap_or(0);
    text.truncate(end);
    text
}

/// The response in `captured`, when gh's exit agrees with it.
fn read(captured: Captured, graphql: bool, token: &GhToken) -> Result<Response, GhError> {
    let exit = captured.status;
    if captured.stdout.is_empty() && !exit.success() {
        return Err(GhError::NoResponse {
            exit,
            stderr: excerpt(&captured.stderr, token),
        });
    }
    let response = reply::parse(captured.stdout).map_err(|detail| GhError::Malformed { detail })?;
    let status = response.status;
    let agrees = match exit.code() {
        Some(0) => (200..300).contains(&status),
        Some(1) => status >= 300 || graphql,
        _ => false,
    };
    if !agrees {
        return Err(GhError::Inconsistent { exit, status });
    }
    Ok(response)
}
