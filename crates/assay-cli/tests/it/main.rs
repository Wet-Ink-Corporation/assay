//! Integration tests for the `assay` binary and the workspace it is the composition root of,
//! compiled as one binary (TST-14).

mod architecture;
mod binary;
#[cfg(target_os = "linux")] // the forge group killer needs /proc and a system `kill`
mod check;
#[cfg(target_os = "linux")] // the forge group killer needs /proc and a system `kill`
mod check_local_events;
#[cfg(target_os = "linux")] // the forge group killer needs /proc and a system `kill`
mod check_weigh_in;
#[cfg(target_os = "linux")] // the forge group killer needs /proc and a system `kill`
mod doctor;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod doctor_prerequisites;
#[cfg(target_os = "linux")] // the forge group killer needs /proc and a system `kill`
mod doctor_profile;
#[cfg(unix)] // process groups and Unix file modes
mod exec_surface;
#[cfg(unix)] // drives a bash script and sets Unix file modes
mod fetch_by_digest;
#[cfg(target_os = "linux")] // the forge group killer needs /proc and a system `kill`
mod gate;
#[cfg(target_os = "linux")] // the forge group killer needs /proc and a system `kill`
mod gate_paths;
#[cfg(target_os = "linux")] // the forge group killer needs /proc and a system `kill`
mod gh;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod layering;
mod layout;
mod open;
mod plan;
#[cfg(target_os = "linux")] // the forge group killer needs /proc and a system `kill`
mod pre_push;
#[cfg(unix)] // process groups, through forge's git
mod pull_request;
mod reconcile;
mod release_workflow;
#[cfg(unix)] // process groups, through `ForgeSupervisor`
mod self_gate;
mod status;
#[cfg(target_os = "linux")] // the forge group killer needs /proc and a system `kill`
mod status_pr;
#[cfg(unix)] // process groups and Unix file modes
mod supervisor;
#[cfg(all(test, unix))] // shared helpers (TST-14); writes Unix executables
mod support;
#[cfg(unix)] // process groups, Unix file modes and a `/bin/sh` stand-in for node
mod temper;
#[cfg(unix)] // drives a bash script and Unix symlinks
mod warm_target;
mod warm_target_workflow;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod workflow;
mod workflow_cache;
