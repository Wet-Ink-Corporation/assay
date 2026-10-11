//! The environment a gate process may see (`docs/architecture/03-contracts.md` §2).

use std::ffi::{OsStr, OsString};

/// Exact names passed through from the inherited environment.
const ALLOWED: [&str; 3] = ["PATH", "HOME", "CI"];

/// Prefix of the assay variables passed through (`ASSAY_BASE_SHA`, `ASSAY_HEAD_SHA`,
/// `ASSAY_RUN_KEY`).
const ASSAY_PREFIX: &str = "ASSAY_";

/// The inherited variables a gate process may see: `PATH`, `HOME`, `CI` and `ASSAY_*`.
///
/// Names match exactly and case-sensitively. Everything else, tokens included, is dropped.
/// Capture it once at the composition root from `std::env::vars_os()`.
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
}

/// Whether a variable named `name` may reach a gate.
fn allowed(name: &OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        ALLOWED.contains(&name)
            || name
                .strip_prefix(ASSAY_PREFIX)
                .is_some_and(|rest| !rest.is_empty())
    })
}
