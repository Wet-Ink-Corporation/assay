//! Integration tests for the `assay` binary and the workspace it is the composition root of,
//! compiled as one binary (TST-14).

mod architecture;
mod binary;
#[cfg(target_os = "linux")] // the forge group killer needs /proc and a system `kill`
mod doctor;
#[cfg(unix)] // process groups and Unix file modes
mod exec_surface;
#[cfg(unix)] // drives a bash script and sets Unix file modes
mod fetch_by_digest;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod layering;
mod layout;
mod plan;
mod release_workflow;
#[cfg(unix)] // process groups, through `ForgeSupervisor`
mod self_gate;
#[cfg(unix)] // process groups and Unix file modes
mod supervisor;
#[cfg(unix)] // process groups, Unix file modes and a `/bin/sh` stand-in for node
mod temper;
