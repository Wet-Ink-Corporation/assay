//! Integration tests for `assay-domain`, compiled as one binary (TST-14).

mod decide;
mod decide_commands;
mod decide_llm;
mod decide_not_run;
mod decide_properties;
mod decide_waive;
mod finding;
mod golden;
mod round_trip;
mod rubric;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod support;
mod vocabulary;
