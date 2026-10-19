//! `assay status [--json] [--pr <n>] [--full]` (#126): the arguments and the wiring of
//! [`assay_app::status`] over its ports, generic so it is tested with fakes; the binary runs
//! it over forge's GitHub adapter through `gh` (#273). `assay status [--json] --local` (#130)
//! shows the local check's result instead; [`StatusCommand`] tells the two apart.

use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};
use std::num::NonZeroU64;

use assay_app::{
    AppId, BundleSource, CommitId, ContractValidator, Detail, FetchedAt, Identities, PullSelector,
    StatusFailure, StatusForge, StatusPorts, StatusRepository, StatusRequest, status,
};
use assay_domain::Rubric;

/// How the status is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    /// Plain text for a person (the default).
    Text,
    /// `--json`: the `assay.status/1` document.
    Json,
}

/// The parsed arguments of `assay status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusArgs {
    /// `--json` or text.
    pub format: Format,
    /// `--pr <n>`; without it, the open PR of the current branch.
    pub pr: Option<NonZeroU64>,
    /// `--full` or not.
    pub detail: Detail,
}

/// Why the arguments of `assay status` were refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusUsage {
    /// An argument that is not `--json`, `--pr` or `--full`.
    Unknown,
    /// A flag given twice.
    Repeated(&'static str),
    /// `--pr` without a positive number after it.
    PrNumber,
    /// A PR's argument (`--pr`, `--full`) beside `--local`.
    NotLocal(&'static str),
}

impl fmt::Display for StatusUsage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let why = match self {
            Self::Unknown => "unknown argument",
            Self::Repeated(flag) => return write!(f, "{flag} given twice; {USAGE}"),
            Self::PrNumber => "--pr needs a PR number",
            Self::NotLocal(flag) => return write!(f, "{flag} does not go with --local; {USAGE}"),
        };
        write!(f, "{why}; {USAGE}")
    }
}

impl std::error::Error for StatusUsage {}

/// The usage line.
const USAGE: &str =
    "usage: assay status [--json] [--pr <n>] [--full], or assay status [--json] --local";

/// What `assay status` is asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusCommand {
    /// A PR's status.
    Pull(StatusArgs),
    /// `--local`: the local check's result, written as `format` says.
    Local(Format),
}

impl StatusCommand {
    /// Parses the arguments after `status`: [`StatusArgs`], or `--local` with `--json` at most.
    ///
    /// # Errors
    ///
    /// [`StatusUsage`] as [`StatusArgs::parse`] refuses, for `--local` given twice, and for
    /// `--pr` or `--full` beside `--local`.
    pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self, StatusUsage> {
        let mut local = None;
        let mut rest = Vec::new();
        for arg in args {
            if arg == "--local" {
                set(&mut local, (), "--local")?;
            } else {
                rest.push(arg);
            }
        }
        let parsed = StatusArgs::parse(rest)?;
        if local.is_none() {
            return Ok(Self::Pull(parsed));
        }
        if parsed.pr.is_some() {
            return Err(StatusUsage::NotLocal("--pr"));
        }
        if parsed.detail == Detail::Full {
            return Err(StatusUsage::NotLocal("--full"));
        }
        Ok(Self::Local(parsed.format))
    }
}

impl StatusArgs {
    /// Parses the arguments after `status`, each flag at most once.
    ///
    /// # Errors
    ///
    /// [`StatusUsage`] for an unknown argument, a repeated flag or a `--pr` without a positive
    /// number.
    pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self, StatusUsage> {
        let mut format = None;
        let mut pr = None;
        let mut detail = None;
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("--json") => set(&mut format, Format::Json, "--json")?,
                Some("--full") => set(&mut detail, Detail::Full, "--full")?,
                Some("--pr") => {
                    let number = args
                        .next()
                        .and_then(|value| value.to_str().and_then(pr_number))
                        .ok_or(StatusUsage::PrNumber)?;
                    set(&mut pr, number, "--pr")?;
                }
                _ => return Err(StatusUsage::Unknown),
            }
        }
        Ok(Self {
            format: format.unwrap_or(Format::Text),
            pr,
            detail: detail.unwrap_or(Detail::Summary),
        })
    }
}

/// Sets `slot` to `value` unless `flag` already set it.
fn set<T>(slot: &mut Option<T>, value: T, flag: &'static str) -> Result<(), StatusUsage> {
    if slot.is_some() {
        return Err(StatusUsage::Repeated(flag));
    }
    *slot = Some(value);
    Ok(())
}

/// A PR number: ASCII digits only, not zero.
fn pr_number(text: &str) -> Option<NonZeroU64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// What the composition root knows besides the arguments: the repository, the App, the local
/// checkout, base config, the time and the contract validator.
#[derive(Debug, Clone, Copy)]
pub struct StatusContext<'a> {
    /// The repository.
    pub repository: &'a StatusRepository,
    /// The `assay-verdict` App's id.
    pub app: AppId,
    /// The PR, chosen once by the composition root: `--pr`'s number, the open PR of the
    /// checkout's branch without it, or one it already read ([`assay_app::read_pull`]).
    pub pull: PullSelector<'a>,
    /// The local checkout's HEAD, if any.
    pub local_head: Option<&'a CommitId>,
    /// The rubric.
    pub rubric: &'a Rubric,
    /// The base P10 identity lists.
    pub identities: &'a Identities,
    /// When the state is read.
    pub fetched_at: &'a FetchedAt,
    /// The validator the document is checked against before it is written.
    pub validator: &'a ContractValidator,
}

/// Why `assay status` printed nothing.
#[derive(Debug)]
pub enum StatusRunError<E> {
    /// The status could not be read.
    Status(E),
    /// It could not be written.
    Write(io::Error),
}

impl<E: fmt::Display> fmt::Display for StatusRunError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Status(error) => write!(f, "no status: {error}"),
            Self::Write(_) => f.write_str("could not write the status"),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for StatusRunError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Status(error) => Some(error),
            Self::Write(error) => Some(error),
        }
    }
}

/// Reads the status of the PR `context` names, in `args`' detail, through `ports` and writes
/// it to `out` in `args`' format (pass `&mut out` to keep the writer).
///
/// # Errors
///
/// [`StatusRunError::Status`] when [`status`] refuses, [`StatusRunError::Write`] when `out`
/// fails.
pub fn run<F: StatusForge, B: BundleSource>(
    args: &StatusArgs,
    context: &StatusContext<'_>,
    ports: &StatusPorts<'_, F, B>,
    mut out: impl Write,
) -> Result<(), StatusRunError<StatusFailure<F, B>>> {
    let request = StatusRequest {
        repository: context.repository,
        app: context.app,
        pull: context.pull,
        local_head: context.local_head,
        detail: args.detail,
        rubric: context.rubric,
        identities: context.identities,
        fetched_at: context.fetched_at,
    };
    let report = status(&request, ports, context.validator).map_err(StatusRunError::Status)?;
    let bytes = match args.format {
        Format::Json => report.json(),
        Format::Text => report.text().as_bytes(),
    };
    out.write_all(bytes)
        .and_then(|()| out.flush())
        .map_err(StatusRunError::Write)
}
