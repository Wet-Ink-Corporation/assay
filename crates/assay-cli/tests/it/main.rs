//! Integration tests for the `assay` binary and the workspace it is the composition root of,
//! compiled as one binary (TST-14).

mod architecture;
mod binary;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod layering;
mod layout;
