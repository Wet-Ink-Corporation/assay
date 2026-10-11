//! assay domain: findings, severities, fingerprints, rubric, tiers, the run state machine and
//! its events.
//!
//! Pure: no I/O, no clock, no environment, no async runtime. Fingerprints are computed here and
//! only here (`docs/architecture/02-architecture.md` §2).

mod aeu;
mod disposition;

pub use aeu::Aeu;
pub use disposition::Disposition;
