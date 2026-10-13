//! Integration tests for `assay-app`, compiled as one binary (TST-14).

mod contract;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod support;
