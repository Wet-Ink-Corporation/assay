//! Integration tests for the `assay` binary and the workspace it is the composition root of,
//! compiled as one binary (TST-14).

mod architecture;
mod binary;
#[cfg(unix)] // drives a bash script and sets Unix file modes
mod fetch_by_digest;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod layering;
mod layout;
mod release_workflow;
#[cfg(unix)] // process groups and Unix file modes
mod supervisor;
