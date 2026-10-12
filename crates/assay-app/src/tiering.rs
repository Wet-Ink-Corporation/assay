//! Tiering inputs that need parsing (`docs/architecture/04-pipeline.md` §2.2): the trust keys
//! and exec roots of TOML-parsed manifests and the temper baseline ratchet. The caller reads
//! both sides at `base_sha` and `head_sha`; the domain's `tier` reads the results.

mod baseline;
mod exec_roots;
mod manifest;
mod packages;

pub use baseline::baseline_change;
pub use exec_roots::{exec_roots, manifest_exec_roots};
pub use manifest::manifest_trust_keys;
pub use packages::{ExecSurface, MetadataError, PackageSet, exec_surface};
