//! Integration tests for `assay-forge`, compiled as one binary (TST-14).

mod attributes;
mod diff;
#[cfg(unix)] // symlinks in fixtures
mod manifest_copy;
mod repository;
mod single_constructor;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod support;
mod tree;
