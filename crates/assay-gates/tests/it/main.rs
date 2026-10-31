//! Integration tests for `assay-gates`, compiled as one binary (TST-14).

mod cargo;
mod environment;
mod exec;
mod reach;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod support;
mod temper;
mod temper_blind;
mod temper_config;
mod temper_exception;
mod temper_inventory;
mod temper_lint;
mod temper_manifest;
mod temper_steps_only;
#[cfg(test)]
mod temper_support;
mod temper_test;
mod weigh_in;
#[cfg(test)]
mod weigh_in_support;
