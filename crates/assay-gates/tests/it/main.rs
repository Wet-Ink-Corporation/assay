//! Integration tests for `assay-gates`, compiled as one binary (TST-14).

mod environment;
mod exec;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod support;
mod temper;
mod temper_config;
mod temper_exception;
#[cfg(test)]
mod temper_support;
