//! Mechanical validation of reviewer findings, 04 §4 steps 1 to 6 (`assay_app::validate`).

mod egress;
mod evidence;
#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod fixture;
mod placement;
mod properties;
mod schema;
mod screen;
mod suppression;
mod trigger;
mod zones;
