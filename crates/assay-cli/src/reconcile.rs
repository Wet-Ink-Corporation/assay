//! `assay reconcile [--ledger]` (06 §4): the per-repo sweep of a consumer repository's own
//! verdict checks, or with `--ledger` the single ledger-side sweep of leases, locks and cost
//! back-fills. The use cases are `assay_app::reconcile_checks` and `reconcile_ledger`; this
//! checks the arguments and the run attempt, then the binary stops, since the live GitHub
//! transport the adapters run through is #273's.

use std::ffi::OsString;
use std::fmt;

/// The variable GitHub Actions numbers a workflow run's attempts in.
pub(crate) const RUN_ATTEMPT: &str = "GITHUB_RUN_ATTEMPT";

/// Which sweep is asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sweep {
    /// The consumer repository's own checks.
    Checks,
    /// The ledger repository's leases, locks and back-fills.
    Ledger,
}

/// Why `assay reconcile` does not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReconcileRefused {
    /// The arguments are not `[--ledger]`.
    Usage,
    /// `GITHUB_RUN_ATTEMPT` is unset or not a number (R3-05: only a first attempt runs).
    NoAttempt,
    /// A re-run (R3-05, 02 §1): a fresh run is the recovery path.
    ReRun(String),
    /// The sweep is ready to run but its transport is not built.
    NotBuilt(Sweep),
}

impl fmt::Display for ReconcileRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage => f.write_str("usage: assay reconcile [--ledger]"),
            Self::NoAttempt => write!(
                f,
                "reconcile: {RUN_ATTEMPT} must be set to the run's attempt number"
            ),
            Self::ReRun(attempt) => write!(
                f,
                "reconcile: run attempt {attempt} is a re-run; only attempt 1 reconciles (R3-05)"
            ),
            Self::NotBuilt(sweep) => {
                let which = match sweep {
                    Sweep::Checks => "per-repo",
                    Sweep::Ledger => "ledger-side",
                };
                write!(
                    f,
                    "reconcile: the live GitHub transport the {which} sweep runs through is not \
                     built yet (#273)"
                )
            }
        }
    }
}

/// The sweep the arguments after `reconcile` ask for, once the run attempt `attempt` is the
/// first: checked before anything is read or written.
///
/// # Errors
///
/// [`ReconcileRefused::Usage`], [`ReconcileRefused::NoAttempt`] or [`ReconcileRefused::ReRun`].
pub(crate) fn prepare(
    args: impl IntoIterator<Item = OsString>,
    attempt: Option<OsString>,
) -> Result<Sweep, ReconcileRefused> {
    let sweep = parse(args)?;
    let attempt = attempt.ok_or(ReconcileRefused::NoAttempt)?;
    let attempt = attempt.to_str().ok_or(ReconcileRefused::NoAttempt)?;
    if attempt.parse::<u64>().is_err() {
        return Err(ReconcileRefused::NoAttempt);
    }
    if attempt != "1" {
        return Err(ReconcileRefused::ReRun(attempt.to_owned()));
    }
    Ok(sweep)
}

/// `[--ledger]`, nothing else.
fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Sweep, ReconcileRefused> {
    let mut args = args.into_iter();
    match (args.next(), args.next()) {
        (None, _) => Ok(Sweep::Checks),
        (Some(flag), None) if flag == "--ledger" => Ok(Sweep::Ledger),
        _ => Err(ReconcileRefused::Usage),
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::{ReconcileRefused, Sweep, prepare};

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    /// `prepare` for the arguments `list` at run attempt `attempt`.
    fn at(list: &[&str], attempt: &str) -> Result<Sweep, ReconcileRefused> {
        prepare(args(list), Some(OsString::from(attempt)))
    }

    #[test]
    fn a_first_attempt_reaches_the_sweep_it_names() {
        assert_eq!(at(&[], "1"), Ok(Sweep::Checks));
        assert_eq!(at(&["--ledger"], "1"), Ok(Sweep::Ledger));
    }

    #[test]
    fn a_re_run_is_refused() {
        assert_eq!(
            at(&["--ledger"], "2"),
            Err(ReconcileRefused::ReRun("2".to_owned()))
        );
        assert_eq!(at(&[], "01"), Err(ReconcileRefused::ReRun("01".to_owned())));
    }

    #[test]
    fn a_missing_or_unreadable_attempt_is_refused() {
        assert_eq!(prepare(args(&[]), None), Err(ReconcileRefused::NoAttempt));
        assert_eq!(at(&[], "one"), Err(ReconcileRefused::NoAttempt));
        assert_eq!(at(&[], ""), Err(ReconcileRefused::NoAttempt));
    }

    #[test]
    fn other_arguments_are_a_usage_error() {
        for list in [&["--ledger", "--ledger"][..], &["--checks"], &["ledger"]] {
            assert_eq!(at(list, "1"), Err(ReconcileRefused::Usage), "{list:?}");
        }
    }
}
