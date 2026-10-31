//! assay gate adapters: the built-in `temper` and `weigh-in` adapters and the `exec` adapter,
//! configured by `.assay/plugins.toml`.
//!
//! Implements the `GateRunner` port of `assay-app` (`docs/architecture/02-architecture.md` §2).
//! It drives temper and weigh-in as external tools and never links their code (P8). The
//! [`ExecAdapter`] runs `exec` gates in Z2b jobs through a [`Supervisor`] with the
//! [`GateEnvironment`] allowlist. The [`TemperAdapter`] runs temper's Z2s checks in
//! `gates-static`, reading the PR through [`RevisionFiles`] and running the pinned temper scan
//! through a [`CaptureSupervisor`].
//!
//! The [`WeighInAdapter`] reads the head's Weigh-In journals and decision atoms in
//! `gates-static`, in experimental mode: non-required, reporting only (04 §9, #109).
//!
//! [`PinnedCargo`] runs the pinned base cargo on a sanitised manifest-only copy in
//! `gates-static`, through a [`CaptureSupervisor`], for exec-surface detection (#65).

mod capture;
mod cargo;
mod environment;
mod exec;
mod revision;
mod temper;
mod weigh_in;

pub use capture::{CaptureSupervisor, CapturedRun};
pub use cargo::{
    CargoEnvironment, CargoError, CargoLimits, CargoSettings, CargoSettingsError, CargoStep,
    PinnedCargo, PinnedToolchain,
};
pub use environment::GateEnvironment;
pub use exec::{
    ExecAdapter, ExecError, ExecLimits, ExecSettings, ExecSettingsError, FINDINGS_FILE,
    FINDINGS_OUT_VAR, ProcessSpec, Supervisor,
};
pub use revision::{EntryKind, Revision, RevisionFiles, TreeEntry};
pub use temper::{
    CompileTimeReach, RatchetBase, RatchetDegradation, TemperAdapter, TemperError, TemperPaths,
    TemperSettings, TemperSettingsError, TemperStepSettings, TemperTestAdapter, compile_time_reach,
};
pub use weigh_in::{WeighInAdapter, WeighInError, WeighInSettingsError};
