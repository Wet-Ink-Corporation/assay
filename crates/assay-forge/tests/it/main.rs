//! Integration tests for `assay-forge`, compiled as one binary (TST-14).

mod attributes;
mod diff;
mod repository;
mod single_constructor;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod support;
mod tree;
