//! `assay status [--json] [--pr <n>] [--full]` (#126): the arguments and the wiring of
//! [`assay_app::status`] over its ports, generic so it is tested with fakes until the GitHub
//! adapter (#53) exists.

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
}

impl fmt::Display for StatusUsage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let why = match self {
            Self::Unknown => "unknown argument",
            Self::Repeated(flag) => return write!(f, "{flag} given twice; {USAGE}"),
            Self::PrNumber => "--pr needs a PR number",
        };
        write!(f, "{why}; {USAGE}")
    }
}

impl std::error::Error for StatusUsage {}

/// The usage line.
const USAGE: &str = "usage: assay status [--json] [--pr <n>] [--full]";

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
    /// The local checkout's current branch, used without `--pr`.
    pub branch: &'a str,
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

/// Reads the status `args` and `context` ask for through `ports` and writes it to `out` (pass
/// `&mut out` to keep the writer).
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
        pull: args
            .pr
            .map_or(PullSelector::Branch(context.branch), PullSelector::Number),
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
