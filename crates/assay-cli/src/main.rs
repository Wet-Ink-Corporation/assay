//! `assay`: the composition root. It will load configuration, wire the adapters into the use
//! cases and map outcomes to exit codes (`docs/architecture/02-architecture.md` §2). No command
//! exists yet, so it exits successfully without doing anything.

use std::process::ExitCode;

fn main() -> ExitCode {
    ExitCode::SUCCESS
}
