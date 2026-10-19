//! The variables of this process that `gh api` may see: the network path to GitHub and the
//! certificates to trust on it, nothing else (wi-fe594c).

use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;

/// The variables passed on when this process has them, in this order.
const PASSED_THROUGH: [&str; 6] = [
    "HTTPS_PROXY",
    "https_proxy",
    "NO_PROXY",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

/// What gh inherits from this process.
///
/// `HTTPS_PROXY`, `https_proxy`, `NO_PROXY`, `no_proxy`, `SSL_CERT_FILE` and `SSL_CERT_DIR`,
/// each when present, and nothing else. Captured once at the edge (ARC-11), like the git
/// layer's `GitEnvironment`. Its `Debug` shows names only, as a proxy URL may hold
/// credentials.
#[derive(Clone, Default)]
pub struct GhEnvironment {
    passed: Vec<(&'static str, OsString)>,
}

impl GhEnvironment {
    /// The passed-through variables of this process's environment.
    #[must_use]
    pub fn from_process_env() -> Self {
        Self::from_vars(env::vars_os())
    }

    /// The passed-through variables among `vars` (the first of a repeated name wins); every
    /// other variable is dropped.
    #[must_use]
    pub fn from_vars(vars: impl IntoIterator<Item = (OsString, OsString)>) -> Self {
        let mut passed: Vec<(&'static str, OsString)> = Vec::new();
        for (name, value) in vars {
            let known = PASSED_THROUGH.iter().find(|&&known| name == known);
            if let Some(&known) = known
                && !passed.iter().any(|&(seen, _)| seen == known)
            {
                passed.push((known, value));
            }
        }
        passed.sort_by_key(|&(name, _)| PASSED_THROUGH.iter().position(|&known| known == name));
        Self { passed }
    }

    /// The variables, in [`PASSED_THROUGH`] order.
    pub(super) fn vars(&self) -> impl Iterator<Item = (&'static str, &OsStr)> + '_ {
        self.passed
            .iter()
            .map(|(name, value)| (*name, value.as_os_str()))
    }
}

impl fmt::Debug for GhEnvironment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("GhEnvironment")
            .field(
                &self
                    .passed
                    .iter()
                    .map(|&(name, _)| name)
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}
