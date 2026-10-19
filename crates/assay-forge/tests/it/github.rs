//! The GitHub adapter against recorded API responses; no network (TST-10).

mod backoff;
mod checks;
mod comments;
mod identity;
mod pulls;
mod reads;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod recorded;
mod status;
mod token;
