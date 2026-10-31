//! The workflow run a command belongs to, from the variables GitHub Actions sets, and the
//! first-attempt guard every command holding a write token passes first (R3-05, 02 §1).

use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::num::NonZeroU64;

use assay_domain::RunUid;
use assay_forge::github::Repository;

/// The Actions variables a pipeline command reads, as the environment holds them; `main`
/// reads them once (ARC-11).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunVariables {
    /// `GITHUB_RUN_ID`.
    pub run_id: Option<OsString>,
    /// `GITHUB_RUN_ATTEMPT`.
    pub run_attempt: Option<OsString>,
    /// `GITHUB_REPOSITORY_ID`.
    pub repository_id: Option<OsString>,
    /// `GITHUB_REPOSITORY`, `owner/name`.
    pub repository: Option<OsString>,
}

/// The workflow run and the repository it runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunEnv {
    /// The run: repository id, run id and attempt.
    pub run_uid: RunUid,
    /// The repository, `owner/name`.
    pub repo: Repository,
}

impl RunEnv {
    /// Reads the run from `vars`. Each id is a positive number written in canonical decimal:
    /// ASCII digits with no leading zero, so `01` is refused rather than read as attempt 1
    /// (wi-b5affa). The repository is `owner/name` as [`Repository`] parses it.
    ///
    /// ```
    /// use assay_cli::{RunEnv, RunVariables};
    ///
    /// let vars = RunVariables {
    ///     run_id: Some("4242".into()),
    ///     run_attempt: Some("1".into()),
    ///     repository_id: Some("1066".into()),
    ///     repository: Some("Wet-Ink-Corporation/assay".into()),
    /// };
    /// let first = RunEnv::from_vars(&vars)?.first_attempt()?;
    /// assert_eq!(first.run_env().run_uid.run_id.get(), 4242);
    /// # Ok::<(), assay_cli::RunEnvError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// [`RunEnvError::Missing`] for an unset variable and [`RunEnvError::Invalid`] for one
    /// that does not parse; each names the variable and never shows its value.
    pub fn from_vars(vars: &RunVariables) -> Result<Self, RunEnvError> {
        let run_uid = RunUid {
            repo_id: positive(vars.repository_id.as_deref(), REPOSITORY_ID)?,
            run_id: positive(vars.run_id.as_deref(), RUN_ID)?,
            run_attempt: positive(vars.run_attempt.as_deref(), RUN_ATTEMPT)?,
        };
        let repo = present(vars.repository.as_deref(), REPOSITORY)?
            .to_str()
            .and_then(|text| text.parse().ok())
            .ok_or(RunEnvError::Invalid(REPOSITORY))?;
        Ok(Self { run_uid, repo })
    }

    /// This run, once it is shown to be the run's first attempt: the only attempt a command
    /// holding a write token runs in; recovery is a fresh run (R3-05, 02 §1). It consumes
    /// `self`, so such a command holds a [`FirstAttempt`] or nothing (API-13).
    ///
    /// # Errors
    ///
    /// [`NotFirstAttempt`] for any attempt but 1.
    pub fn first_attempt(self) -> Result<FirstAttempt, NotFirstAttempt> {
        let attempt = self.run_uid.run_attempt;
        if attempt.get() == 1 {
            Ok(FirstAttempt(self))
        } else {
            Err(NotFirstAttempt { attempt })
        }
    }
}

/// A [`RunEnv`] whose attempt is 1; only [`RunEnv::first_attempt`] makes one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstAttempt(RunEnv);

impl FirstAttempt {
    /// The run.
    #[must_use]
    pub const fn run_env(&self) -> &RunEnv {
        &self.0
    }

    /// The run, giving up the proof that it is a first attempt.
    #[must_use]
    pub fn into_run_env(self) -> RunEnv {
        self.0
    }
}

/// The run is a re-run: a command holding a write token runs only on attempt 1 (R3-05).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotFirstAttempt {
    /// The attempt.
    pub attempt: NonZeroU64,
}

impl fmt::Display for NotFirstAttempt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "run attempt {} is a re-run: only a first attempt runs; start a new run (R3-05)",
            self.attempt
        )
    }
}

impl Error for NotFirstAttempt {}

/// Why the run could not be read, or may not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunEnvError {
    /// The variable is not set.
    Missing(&'static str),
    /// The variable does not parse; its value is not shown.
    Invalid(&'static str),
    /// The run is not its first attempt.
    NotFirstAttempt(NotFirstAttempt),
}

impl From<NotFirstAttempt> for RunEnvError {
    fn from(refusal: NotFirstAttempt) -> Self {
        Self::NotFirstAttempt(refusal)
    }
}

impl fmt::Display for RunEnvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(name) => write!(f, "{name} is not set"),
            Self::Invalid(name) if *name == REPOSITORY => {
                write!(f, "{name} is not a repository's `owner/name`")
            }
            Self::Invalid(name) => write!(
                f,
                "{name} is not a positive number in decimal without leading zeros"
            ),
            Self::NotFirstAttempt(_) => f.write_str("the run may not run"),
        }
    }
}

impl Error for RunEnvError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::NotFirstAttempt(refusal) => Some(refusal),
            Self::Missing(_) | Self::Invalid(_) => None,
        }
    }
}

/// `GITHUB_RUN_ID`.
const RUN_ID: &str = "GITHUB_RUN_ID";
/// `GITHUB_RUN_ATTEMPT`.
const RUN_ATTEMPT: &str = "GITHUB_RUN_ATTEMPT";
/// `GITHUB_REPOSITORY_ID`.
const REPOSITORY_ID: &str = "GITHUB_REPOSITORY_ID";
/// `GITHUB_REPOSITORY`.
const REPOSITORY: &str = "GITHUB_REPOSITORY";

/// Variable `name`'s value, which must be set.
fn present<'v>(value: Option<&'v OsStr>, name: &'static str) -> Result<&'v OsStr, RunEnvError> {
    value.ok_or(RunEnvError::Missing(name))
}

/// Variable `name`'s value as a positive number in canonical decimal.
fn positive(value: Option<&OsStr>, name: &'static str) -> Result<NonZeroU64, RunEnvError> {
    present(value, name)?
        .to_str()
        .and_then(canonical_positive)
        .ok_or(RunEnvError::Invalid(name))
}

/// `text` as a positive number: ASCII digits, the first not `0`, within `u64`.
pub(crate) fn canonical_positive(text: &str) -> Option<NonZeroU64> {
    let canonical = text
        .bytes()
        .next()
        .is_some_and(|first| matches!(first, b'1'..=b'9'))
        && text.bytes().all(|byte| byte.is_ascii_digit());
    if canonical { text.parse().ok() } else { None }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn variables(attempt: Option<&str>) -> RunVariables {
        RunVariables {
            run_id: Some("4242".into()),
            run_attempt: attempt.map(OsString::from),
            repository_id: Some("1066".into()),
            repository: Some("Wet-Ink-Corporation/assay".into()),
        }
    }

    fn id(value: u64) -> NonZeroU64 {
        NonZeroU64::new(value).expect("a positive test id")
    }

    #[test]
    fn reads_the_run_and_the_repository() {
        let env = RunEnv::from_vars(&variables(Some("3")));

        assert_eq!(
            env,
            Ok(RunEnv {
                run_uid: RunUid {
                    repo_id: id(1066),
                    run_id: id(4242),
                    run_attempt: id(3),
                },
                repo: "Wet-Ink-Corporation/assay".parse().expect("a valid name"),
            })
        );
    }

    #[test]
    fn names_each_missing_variable() {
        type Unset = fn(&mut RunVariables);
        let cases: [(Unset, &str); 4] = [
            (|vars| vars.run_id = None, "GITHUB_RUN_ID"),
            (|vars| vars.run_attempt = None, "GITHUB_RUN_ATTEMPT"),
            (|vars| vars.repository_id = None, "GITHUB_REPOSITORY_ID"),
            (|vars| vars.repository = None, "GITHUB_REPOSITORY"),
        ];
        for (unset, name) in cases {
            let mut vars = variables(Some("1"));
            unset(&mut vars);

            assert_eq!(
                RunEnv::from_vars(&vars),
                Err(RunEnvError::Missing(name)),
                "{name}"
            );
        }
    }

    #[test]
    fn refuses_a_zero_leading_zero_or_non_decimal_id_by_name() {
        for value in [
            "0",
            "01",
            "",
            "+1",
            " 1",
            "1 ",
            "0x10",
            "١",
            "18446744073709551616",
        ] {
            let mut vars = variables(Some("1"));
            vars.run_id = Some(value.into());

            assert_eq!(
                RunEnv::from_vars(&vars),
                Err(RunEnvError::Invalid("GITHUB_RUN_ID")),
                "{value:?}"
            );
        }
    }

    #[test]
    fn refuses_a_repository_that_is_not_owner_and_name() {
        let mut vars = variables(Some("1"));
        vars.repository = Some("Wet-Ink-Corporation/assay/extra".into());

        assert_eq!(
            RunEnv::from_vars(&vars),
            Err(RunEnvError::Invalid("GITHUB_REPOSITORY"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_non_unicode_variable_as_invalid() {
        use std::os::unix::ffi::OsStringExt;

        let mut vars = variables(Some("1"));
        vars.repository_id = Some(OsString::from_vec(vec![b'1', 0xff]));

        assert_eq!(
            RunEnv::from_vars(&vars),
            Err(RunEnvError::Invalid("GITHUB_REPOSITORY_ID"))
        );
    }

    #[test]
    fn the_error_never_shows_the_value() {
        let mut vars = variables(Some("1"));
        vars.run_id = Some("secret-looking-value".into());

        let message = RunEnv::from_vars(&vars)
            .expect_err("an invalid run id")
            .to_string();

        assert!(!message.contains("secret-looking-value"), "{message}");
    }

    #[test]
    fn first_attempt_passes_attempt_one() {
        let env = RunEnv::from_vars(&variables(Some("1"))).expect("valid variables");

        let first = env.clone().first_attempt();

        assert_eq!(first.map(FirstAttempt::into_run_env), Ok(env));
    }

    #[test]
    fn first_attempt_refuses_a_second_attempt() {
        let env = RunEnv::from_vars(&variables(Some("2"))).expect("valid variables");

        assert_eq!(env.first_attempt(), Err(NotFirstAttempt { attempt: id(2) }));
    }

    #[test]
    fn an_unset_or_zero_padded_attempt_never_reaches_a_first_attempt() {
        for attempt in [None, Some("01"), Some("1.0")] {
            let first =
                RunEnv::from_vars(&variables(attempt)).and_then(|env| Ok(env.first_attempt()?));

            assert_eq!(
                first,
                Err(if attempt.is_some() {
                    RunEnvError::Invalid("GITHUB_RUN_ATTEMPT")
                } else {
                    RunEnvError::Missing("GITHUB_RUN_ATTEMPT")
                }),
                "{attempt:?}"
            );
        }
    }

    proptest! {
        #[test]
        fn only_the_attempt_spelled_1_is_a_first_attempt(attempt in "[0-9+ a-z]{0,6}") {
            let first = RunEnv::from_vars(&variables(Some(&attempt)))
                .and_then(|env| Ok(env.first_attempt()?));

            prop_assert_eq!(first.is_ok(), attempt == "1", "{:?}", attempt);
        }

        #[test]
        fn every_canonical_attempt_above_1_is_refused_with_its_number(attempt in 2..=u64::MAX) {
            let first = RunEnv::from_vars(&variables(Some(&attempt.to_string())))
                .and_then(|env| Ok(env.first_attempt()?));

            prop_assert_eq!(
                first,
                Err(RunEnvError::NotFirstAttempt(NotFirstAttempt { attempt: id(attempt) }))
            );
        }
    }
}
