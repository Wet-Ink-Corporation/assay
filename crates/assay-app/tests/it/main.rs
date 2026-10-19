//! Integration tests for `assay-app`, compiled as one binary (TST-14).

mod builtins;
mod check;
mod check_egress;
mod check_events;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod check_fakes;
mod check_patches;
mod config;
mod contract;
mod coordinate;
mod decide;
mod decide_evidence;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod decide_fakes;
mod decide_refusals;
mod doctor;
mod domain_model;
mod eval_compare;
mod eval_corpus;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod eval_fakes;
mod eval_run;
mod exec_roots;
mod gate;
mod golden;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod lease_fakes;
mod lease_grant;
mod lease_lock;
mod lease_slot;
mod ledger_budget;
mod ledger_cost;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod ledger_fakes;
mod ledger_schema;
mod ledger_state;
mod ledger_streams;
mod ledger_time;
mod model_runner;
mod open;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod open_fakes;
mod package_set;
mod plan;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod plan_fakes;
mod preflight;
mod prompt_bundle;
mod publish;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod publish_fakes;
mod publish_findings;
mod publish_persist;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod publish_support;
mod read_limits;
mod reconcile_checks;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod reconcile_fakes;
mod reconcile_ledger;
mod redaction;
mod replay;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod replay_fakes;
mod rereview_anchor;
mod rereview_carried;
mod rereview_carry;
mod rereview_settle;
mod rereview_target;
mod self_trust;
mod session_init;
mod shard;
mod shard_runs;
mod spec;
mod specialists;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod specialists_fakes;
mod status;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod status_fakes;
mod status_local;
mod status_refusals;
mod status_refused;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod support;
mod tiering;
mod token_expiry;
mod validation;
mod verify;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod verify_fakes;
mod verify_full;
