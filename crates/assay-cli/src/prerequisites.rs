//! `assay doctor --repository <owner/name> --app <id> --ledger-app <id>` (#75): the arguments
//! and the wiring of [`assay_app::prerequisites`] over any [`PrerequisiteForge`], generic so it
//! is tested with fakes until the live `gh` transport (#273) exists.
//!
//! [`run_prerequisites`] reads base config from the repository's default branch, connects to
//! the ledger repository its `[assay].ledger_repo` names, and judges every prerequisite;
//! [`write_prerequisites`] writes one line per prerequisite. The binary exits 0 when every
//! prerequisite holds and 1 when one fails ([`prerequisites_exit_code`]), and 2 when they
//! cannot be judged.

use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};
use std::num::NonZeroU64;
use std::process::ExitCode;

use assay_app::{
    AppId, BaseError, PrerequisiteApps, PrerequisiteForge, PrerequisiteReport, prerequisites,
    read_base,
};
use assay_forge::github::Repository;

/// The usage line.
const USAGE: &str = "usage: assay doctor --repository <owner/name> --app <id> --ledger-app <id>";

/// The parsed arguments of `assay doctor --repository`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrerequisitesArgs {
    /// `--repository`: the repository to judge.
    pub repository: Repository,
    /// `--app` and `--ledger-app`: the `assay-verdict` and `assay-ledger-writer` Apps.
    pub apps: PrerequisiteApps,
}

/// Why the arguments of `assay doctor --repository` were refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrerequisitesUsage {
    /// An argument that is not one of the three flags.
    Unknown,
    /// A flag given twice.
    Repeated(&'static str),
    /// A flag without a valid value after it.
    Value(&'static str),
    /// A flag that is required and missing.
    Missing(&'static str),
}

impl fmt::Display for PrerequisitesUsage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown => write!(f, "unknown argument; {USAGE}"),
            Self::Repeated(flag) => write!(f, "{flag} given twice; {USAGE}"),
            Self::Value(flag) => write!(f, "{flag} needs a valid value; {USAGE}"),
            Self::Missing(flag) => write!(f, "{flag} is required; {USAGE}"),
        }
    }
}

impl Error for PrerequisitesUsage {}

impl PrerequisitesArgs {
    /// Whether the arguments after `doctor` ask for the repository checks rather than the
    /// session checks: the first one is a flag.
    #[must_use]
    pub fn requested(first: Option<&OsString>) -> bool {
        first.is_some_and(|arg| arg.to_str().is_some_and(|arg| arg.starts_with("--")))
    }

    /// Parses the arguments after `doctor`, each flag exactly once.
    ///
    /// # Errors
    ///
    /// [`PrerequisitesUsage`] for an unknown argument, a repeated or missing flag, a
    /// repository that is not `owner/name`, or an App id that is not a positive number.
    pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self, PrerequisitesUsage> {
        let mut repository = None;
        let mut verdict = None;
        let mut ledger = None;
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            let value = args.next();
            let value = value.as_ref().and_then(|value| value.to_str());
            match arg.to_str() {
                Some("--repository") => {
                    let parsed = value.and_then(|value| value.parse::<Repository>().ok());
                    set(&mut repository, parsed, "--repository")?;
                }
                Some("--app") => set(&mut verdict, value.and_then(app_id), "--app")?,
                Some("--ledger-app") => {
                    set(&mut ledger, value.and_then(app_id), "--ledger-app")?;
                }
                _ => return Err(PrerequisitesUsage::Unknown),
            }
        }
        Ok(Self {
            repository: repository.ok_or(PrerequisitesUsage::Missing("--repository"))?,
            apps: PrerequisiteApps {
                verdict: verdict.ok_or(PrerequisitesUsage::Missing("--app"))?,
                ledger: ledger.ok_or(PrerequisitesUsage::Missing("--ledger-app"))?,
            },
        })
    }
}

/// Sets `slot` to `value` unless `flag` already set it, refusing a missing value.
fn set<T>(
    slot: &mut Option<T>,
    value: Option<T>,
    flag: &'static str,
) -> Result<(), PrerequisitesUsage> {
    if slot.is_some() {
        return Err(PrerequisitesUsage::Repeated(flag));
    }
    *slot = Some(value.ok_or(PrerequisitesUsage::Value(flag))?);
    Ok(())
}

/// An App id: ASCII digits only, not zero.
fn app_id(text: &str) -> Option<AppId> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse::<NonZeroU64>().ok().map(AppId)
}

/// Why the prerequisites could not be judged.
#[derive(Debug)]
pub enum PrerequisitesRunError<E, C> {
    /// Base config could not be read.
    Base(BaseError<E>),
    /// The ledger repository could not be connected to.
    Ledger(C),
}

impl<E: Error + 'static, C: Error + 'static> fmt::Display for PrerequisitesRunError<E, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Base(error) => {
                f.write_str("base config: ")?;
                write_chain(f, error)
            }
            Self::Ledger(error) => {
                f.write_str("the ledger repository: ")?;
                write_chain(f, error)
            }
        }
    }
}

impl<E: Error + 'static, C: Error + 'static> Error for PrerequisitesRunError<E, C> {}

/// Reads base config from `repository`, connects to the ledger with `connect` when
/// `[assay].ledger_repo` names one, and judges every prerequisite.
///
/// # Errors
///
/// [`PrerequisitesRunError::Base`] when base config cannot be read, and
/// [`PrerequisitesRunError::Ledger`] when `connect` fails. A failed prerequisite is not an
/// error: it is in the report.
pub fn run_prerequisites<F, C, CE>(
    repository: &F,
    connect: C,
    apps: PrerequisiteApps,
) -> Result<PrerequisiteReport<F::Error>, PrerequisitesRunError<F::Error, CE>>
where
    F: PrerequisiteForge,
    C: FnOnce(&str) -> Result<F, CE>,
{
    let base = read_base(repository).map_err(PrerequisitesRunError::Base)?;
    let ledger = base
        .config()
        .assay()
        .ledger_repo
        .as_deref()
        .map(connect)
        .transpose()
        .map_err(PrerequisitesRunError::Ledger)?;
    Ok(prerequisites(repository, ledger.as_ref(), &base, apps))
}

/// The exit code of a judged report: 0 when every prerequisite holds, 1 when one fails. (A
/// report that cannot be judged is a [`PrerequisitesRunError`], exit 2.)
#[must_use]
pub fn prerequisites_exit_code<E>(report: &PrerequisiteReport<E>) -> ExitCode {
    if report.passed() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Writes one line per prerequisite, `ok <name>` or `FAIL <name>: <why>` with every cause.
///
/// # Errors
///
/// When `out` cannot be written.
pub fn write_prerequisites<E: Error + 'static>(
    report: &PrerequisiteReport<E>,
    mut out: impl Write,
) -> io::Result<()> {
    for result in report.results() {
        match &result.failure {
            None => writeln!(out, "ok {}", result.prerequisite)?,
            Some(failure) => writeln!(out, "FAIL {}: {}", result.prerequisite, Chain(failure))?,
        }
    }
    out.flush()
}

/// An error with each of its causes, joined by `: `.
struct Chain<'e>(&'e dyn Error);

impl fmt::Display for Chain<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_chain(f, self.0)
    }
}

/// `error` and each of its causes, joined by `: `.
fn write_chain(f: &mut fmt::Formatter<'_>, error: &dyn Error) -> fmt::Result {
    write!(f, "{error}")?;
    let mut cause = error.source();
    while let Some(next) = cause {
        write!(f, ": {next}")?;
        cause = next.source();
    }
    Ok(())
}
