//! assay forge adapter: GitHub REST and GraphQL through `gh`, App-token minting, diffs and
//! objects, checks, comments, timeline events, reactions and the ledger repository.
//!
//! Implements the `Forge` and `Ledger` ports of `assay-app`
//! (`docs/architecture/02-architecture.md` §2). [`git`] is the hardened git layer that fetches
//! and measures PR objects as data (`docs/architecture/03-contracts.md` §9). [`process`] is the
//! bounded process-group supervisor under it, which `assay-cli` also lends to the gate and
//! model-runner adapters (wi-603c63). [`github`] is the GitHub adapter that implements the
//! app's GitHub ports over a [`github::Transport`].

pub mod git;
pub mod github;
pub mod process;
