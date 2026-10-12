//! Tiering inputs that need parsing (`docs/architecture/04-pipeline.md` §2.2): the trust keys
//! of TOML-parsed manifests and the temper baseline ratchet. The caller reads both sides at
//! `base_sha` and `head_sha`; the domain's `tier` reads the results.

mod baseline;
mod manifest;

pub use baseline::baseline_change;
pub use manifest::manifest_trust_keys;
