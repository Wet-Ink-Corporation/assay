//! `assay`: the composition root. It loads configuration, wires the adapters into the use cases
//! and maps outcomes to exit codes (`docs/architecture/02-architecture.md` §2).
//!
//! Commands so far: `assay self-gate <corpus-dir> <rubric-file>`, the `assay-self` exec gate
//! (02 §7, `05` T16). With no arguments it exits successfully without doing anything.

mod self_gate;

use std::env;
use std::io::{self, Write};
use std::process::ExitCode;

use assay_domain::GateConclusion;

/// Exit code of a usage error or a gate error (03 §2: 2 and above).
const ERROR: u8 = 2;

fn main() -> ExitCode {
    let mut args = env::args_os().skip(1);
    let Some(command) = args.next() else {
        return ExitCode::SUCCESS;
    };
    if command != "self-gate" {
        return report(&self_gate::SelfGateError::Usage);
    }
    let variables = self_gate::GateVariables {
        findings_out: env::var_os(self_gate::FINDINGS_OUT),
        run_key: env::var_os(self_gate::RUN_KEY),
    };
    // Exit 0 lets the document's conclusion speak and exit 1 fails the gate (03 §2); only a
    // pass exits 0.
    match self_gate::run(args, variables) {
        Ok(GateConclusion::Pass) => ExitCode::SUCCESS,
        Ok(GateConclusion::Fail | GateConclusion::Neutral) => ExitCode::FAILURE,
        Err(error) => report(&error),
    }
}

/// Writes `error` to stderr and returns the error exit code.
fn report(error: &dyn std::fmt::Display) -> ExitCode {
    // stderr is the last channel: a failed write there leaves the exit code as the report.
    match writeln!(io::stderr().lock(), "assay: {error}") {
        Ok(()) | Err(_) => ExitCode::from(ERROR),
    }
}
