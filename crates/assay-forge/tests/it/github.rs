//! The GitHub adapter against recorded API responses; no network (TST-10).

#[cfg(test)] // lets clippy's allow-*-in-tests cover the fake runner, not only #[test] fns
#[cfg(unix)] // raw wait statuses and symlinks
mod artifacts;
mod backoff;
mod checks;
mod comments;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod contents;
mod deadline;
#[cfg(unix)] // exit statuses built from raw wait statuses
mod gh;
mod identity;
mod ledger;
mod ledger_refs;
mod ledger_refusals;
mod pulls;
mod reads;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod reconcile;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod recorded;
mod settings;
mod status;
mod token;
