//! assay gate adapters: the built-in `temper` and `weigh-in` adapters and the `exec` adapter,
//! configured by `.assay/plugins.toml`.
//!
//! Implements the `GateRunner` port of `assay-app` (`docs/architecture/02-architecture.md` §2).
//! It drives temper and weigh-in as external tools and never links their code (P8). The
//! [`ExecAdapter`] runs `exec` gates in Z2b jobs through a [`Supervisor`] with the
//! [`GateEnvironment`] allowlist. The [`TemperAdapter`] runs temper's Z2s checks in
//! `gates-static`, reading the PR through [`RevisionFiles`] and running the pinned temper scan
//! through a [`CaptureSupervisor`].

mod capture;
mod environment;
mod exec;
mod revision;
mod temper;

pub use capture::{CaptureSupervisor, CapturedRun};
pub use environment::GateEnvironment;
pub use exec::{
    ExecAdapter, ExecError, ExecLimits, ExecSettings, ExecSettingsError, FINDINGS_FILE,
    FINDINGS_OUT_VAR, ProcessSpec, Supervisor,
};
pub use revision::{FileContent, Revision, RevisionFiles, TreeFile};
pub use temper::{TemperAdapter, TemperError, TemperSettings, TemperSettingsError};
