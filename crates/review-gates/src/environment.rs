//! The environment a gate process may see (`docs/architecture/03-contracts.md` §2).

use std::env;
use std::ffi::{OsStr, OsString};

/// The names passed through from the inherited environment (03 §2). The assay variables are
/// named one by one, not by prefix, so that no future `REVIEW_*` secret reaches PR-controlled
/// code.
const ALLOWED: [&str; 6] = [
    "PATH",
    "HOME",
    "CI",
    "REVIEW_BASE_SHA",
    "REVIEW_HEAD_SHA",
    "REVIEW_RUN_KEY",
];

/// The inherited variables a gate process may see: `PATH`, `HOME`, `CI`, `REVIEW_BASE_SHA`,
/// `REVIEW_HEAD_SHA` and `REVIEW_RUN_KEY`.
///
/// Names match exactly and case-sensitively. Everything else, tokens and any other `REVIEW_*`
/// variable included, is dropped. Capture it once at the composition root from
/// `std::env::vars_os()`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GateEnvironment {
    vars: Vec<(OsString, OsString)>,
}

impl GateEnvironment {
    /// Keeps the allowlisted variables of `vars`, in their order.
    #[must_use]
    pub fn from_vars(vars: impl IntoIterator<Item = (OsString, OsString)>) -> Self {
        Self {
            vars: vars.into_iter().filter(|(name, _)| allowed(name)).collect(),
        }
    }

    /// The kept variables, as name and value.
    #[must_use]
    pub fn vars(&self) -> &[(OsString, OsString)] {
        &self.vars
    }

    /// Whether every entry of `PATH` is an absolute directory. An empty or relative entry
    /// resolves against the working directory, which for a gate tool is the PR's tree.
    pub(crate) fn search_path_is_absolute(&self) -> bool {
        self.vars
            .iter()
            .filter(|(name, _)| name == "PATH")
            .all(|(_, value)| env::split_paths(value).all(|entry| entry.is_absolute()))
    }
}

/// Whether a variable named `name` may reach a gate.
fn allowed(name: &OsStr) -> bool {
    name.to_str().is_some_and(|name| ALLOWED.contains(&name))
}
