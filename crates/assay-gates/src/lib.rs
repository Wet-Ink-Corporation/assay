//! assay gate adapters: the built-in `temper` and `weigh-in` adapters and the `exec` adapter,
//! configured by `.assay/plugins.toml`.
//!
//! Implements the `GateRunner` port of `assay-app` (`docs/architecture/02-architecture.md` §2).
//! It drives temper and weigh-in as external tools and never links their code (P8). Empty until
//! the first gates story.
