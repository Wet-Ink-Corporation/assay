//! `assay`: the composition root. It loads configuration, wires the adapters into the use cases
//! and maps outcomes to exit codes (`docs/architecture/02-architecture.md` §2).
//!
//! Commands so far: `assay self-gate <corpus-dir> <rubric-file>`, the `assay-self` exec gate
//! (02 §7, `05` T16), `assay doctor <plugins-toml> <claude-program>`, the session checks of
//! 03 §8 (#121), and `assay check --local`, the advisory check of a working tree (02 §6, #107).
//! `assay status [--json] [--pr <n>] [--full]` (#126) checks its arguments and
//! then stops with exit 2: its GitHub adapter is #53's, and [`assay_cli::run_status`] wires it
//! once that exists. With no arguments it exits successfully without doing anything.

mod check;
mod doctor;
mod self_gate;

use std::env;
use std::io::{self, Write};
use std::process::ExitCode;
use std::time::SystemTime;

use assay_domain::GateConclusion;

/// Exit code of a usage error or a gate error (03 §2: 2 and above).
const ERROR: u8 = 2;

fn main() -> ExitCode {
    let mut args = env::args_os().skip(1);
    let Some(command) = args.next() else {
        return ExitCode::SUCCESS;
    };
    if command == "doctor" {
        return run_doctor(args);
    }
    if command == "check" {
        return run_check(args);
    }
    if command == "status" {
        return match assay_cli::StatusArgs::parse(args) {
            Ok(_) => report(&"status: the GitHub adapter it reads through is not built yet (#53)"),
            Err(usage) => report(&usage),
        };
    }
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

/// `assay doctor`: exit 0 when every check passes, 1 when one fails or the token is past its
/// recorded expiry, 2 when they cannot run.
fn run_doctor(args: impl Iterator<Item = std::ffi::OsString>) -> ExitCode {
    let variables = doctor::DoctorVariables {
        token: env::var_os(doctor::OAUTH_TOKEN),
        path: env::var_os("PATH"),
        expires_on: env::var_os(doctor::TOKEN_EXPIRES_ON),
        now: SystemTime::now(),
    };
    let outcome = match doctor::run(args, variables) {
        Ok(outcome) => outcome,
        Err(error) => return report(&error),
    };
    if let Err(error) = doctor::write_report(&outcome, io::stdout().lock()) {
        return report(&error);
    }
    if outcome.passed() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// `assay check --local`: exit 0 when the decision passes, 1 when it does not, 2 when the
/// check cannot run.
fn run_check(args: impl Iterator<Item = std::ffi::OsString>) -> ExitCode {
    let variables = check::CheckVariables {
        cwd: env::current_dir(),
        path: env::var_os("PATH"),
        home: env::var_os("HOME"),
        xdg_config_home: env::var_os("XDG_CONFIG_HOME"),
        vars: env::vars_os().collect(),
    };
    let checked = match check::run(args, variables) {
        Ok(checked) => checked,
        Err(error) => return report(&error),
    };
    if let Err(error) = checked.write(io::stdout().lock()) {
        return report(&error);
    }
    if let Some(left) = &checked.scratch_left {
        return report(left);
    }
    if checked.passed() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Writes `error` to stderr and returns the error exit code.
fn report(error: &dyn std::fmt::Display) -> ExitCode {
    // stderr is the last channel: a failed write there leaves the exit code as the report.
    match writeln!(io::stderr().lock(), "assay: {error}") {
        Ok(()) | Err(_) => ExitCode::from(ERROR),
    }
}
