//! The gate jobs and the trust zone each runs in (`docs/architecture/02-architecture.md` §3).

use std::fmt;

/// A Z2b job: one that builds or tests PR code. Declared steps and `exec` gates run only in
/// these, so a gate that executes PR-controlled code cannot be scheduled into Z2s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuildJob {
    /// `gates-lint`: clippy, rustdoc, `fmt`, `deny` (`job = "lint"`).
    Lint,
    /// `gates-test`: tests and doctests, on a separate VM (`job = "test"`).
    Test,
}

impl BuildJob {
    /// The job's name in `.assay/plugins.toml`, or `None` for any other text.
    pub(crate) fn from_config(name: &str) -> Option<Self> {
        match name {
            "lint" => Some(Self::Lint),
            "test" => Some(Self::Test),
            _ => None,
        }
    }
}

/// The gate job a process is running as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GateJob {
    /// `gates-static`, zone Z2s: no PR-controlled execution.
    Static,
    /// `gates-lint` or `gates-test`, zone Z2b: executes PR code.
    Build(BuildJob),
}

impl fmt::Display for BuildJob {
    /// The job's name: `gates-lint` or `gates-test`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Lint => "gates-lint",
            Self::Test => "gates-test",
        })
    }
}

impl fmt::Display for GateJob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Static => f.write_str("gates-static"),
            Self::Build(job) => job.fmt(f),
        }
    }
}
