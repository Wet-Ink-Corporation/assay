//! Integration tests for `assay-domain`, compiled as one binary (TST-14).

mod content_hash;
mod finding;
mod fingerprint;
mod round_trip;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod support;
mod vocabulary;
