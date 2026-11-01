//! Integration tests for the `assay` binary and the workspace it is the composition root of,
//! compiled as one binary (TST-14).

#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod admit_fakes;
#[cfg(unix)] // the scene's PATH is one directory
mod admit_job;
mod admit_run;
mod architecture;
mod binary;
#[cfg(target_os = "linux")] // the forge group killer needs /proc and a system `kill`
mod check;
#[cfg(target_os = "linux")] // the forge group killer needs /proc and a system `kill`
mod check_local_events;
#[cfg(target_os = "linux")] // the forge group killer needs /proc and a system `kill`
mod check_weigh_in;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod decide_fakes;
mod decide_job;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod decide_only_guard;
#[cfg(target_os = "linux")] // the forge group killer needs /proc and a system `kill`
mod decide_only_job;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod decide_only_template;
mod decide_run;
#[cfg(unix)] // Unix symlinks
mod dir_artifacts;
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
mod gate_lint;
#[cfg(target_os = "linux")] // the forge group killer needs /proc and a system `kill`
mod gate_lint_checkout;
#[cfg(target_os = "linux")] // the forge group killer needs /proc and a system `kill`
mod gate_outputs;
#[cfg(target_os = "linux")] // the forge group killer needs /proc and a system `kill`
mod gate_paths;
#[cfg(target_os = "linux")] // the forge group killer needs /proc and a system `kill`
mod gate_static;
#[cfg(target_os = "linux")] // the forge group killer needs /proc and a system `kill`
mod gate_static_refusals;
#[cfg(target_os = "linux")] // the forge group killer needs /proc and a system `kill`
mod gate_static_scene;
#[cfg(target_os = "linux")] // the forge group killer needs /proc and a system `kill`
mod gh;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod guard;
#[cfg(target_os = "linux")] // the forge group killer needs /proc and a system `kill`
mod guard_job;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod layering;
mod layout;
mod open;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod open_fakes;
#[cfg(target_os = "linux")] // the forge group killer needs /proc and a system `kill`
mod open_job;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod open_run;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod pipeline_argv;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod pipeline_template;
mod plan;
#[cfg(target_os = "linux")] // the forge group killer needs /proc and a system `kill`
mod plan_job;
mod plan_run;
#[cfg(target_os = "linux")] // the forge group killer needs /proc and a system `kill`
mod pre_push;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod publish_run;
#[cfg(unix)] // process groups, through forge's git
mod pull_request;
mod reconcile;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod redecide_fakes;
mod redecide_job;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod redecide_run;
#[cfg(unix)] // stand-in executables with Unix file modes
mod release_job;
mod release_workflow;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod republish_fakes;
mod republish_job;
#[cfg(unix)] // a FIFO as GITHUB_OUTPUT
mod review;
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
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod workflow_template;
