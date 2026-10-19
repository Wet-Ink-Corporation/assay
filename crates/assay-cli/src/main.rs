//! `assay`: the composition root. It loads configuration, wires the adapters into the use cases
//! and maps outcomes to exit codes (`docs/architecture/02-architecture.md` §2).
//!
//! Commands so far: `assay self-gate <corpus-dir> <rubric-file>`, the `assay-self` exec gate
//! (02 §7, `05` T16), `assay doctor <plugins-toml> <claude-program>`, the session checks of
//! 03 §8 (#121), and `assay check --local`, the advisory check of a working tree (02 §6, #107).
//! `assay pre-push` (#108) runs that check's CP1 alone, the Weigh-In checkpoint the pre-push
//! hook in `plugin/hooks/` calls (04 §9).
//! `assay doctor --repository <owner/name> --app <id> --ledger-app <id>` (#75) checks its
//! arguments and then stops with exit 2: [`assay_cli::run_prerequisites`] runs the repository
//! checks of 05 §7, but the binary does not wire it to the live `gh` transport (#273) yet.
//! `assay status [--json] [--pr <n>] [--full]` (#126, #273) shows a PR's status, read live
//! through `gh` with `ASSAY_GITHUB_TOKEN`, from a checkout whose `origin` is on github.com.
//! `assay status [--json] --local` (#130) shows the local check's result.
//! `assay open` (#54) reads its run from the Actions environment, refuses a re-run, and then
//! stops with exit 2: the live transport it writes through is #273's.
//! `assay reconcile [--ledger]` (#73) refuses any run attempt but the first (R3-05), then stops
//! with exit 2 until the live GitHub transport (#273) exists. With no arguments it exits
//! successfully without doing anything.

mod check;
mod doctor;
mod reconcile;
mod self_gate;
mod status_local;
mod status_pr;

use std::env;
use std::io::{self, Write};
use std::process::ExitCode;
use std::time::SystemTime;

use assay_cli::{Format, OpenCommand, OpenVariables, PrerequisitesArgs, StatusArgs, StatusCommand};
use assay_domain::GateConclusion;
use assay_forge::github::GhEnvironment;

/// Exit code of a usage error or a gate error (03 §2: 2 and above).
const ERROR: u8 = 2;

fn main() -> ExitCode {
    let mut args = env::args_os().skip(1);
    let Some(command) = args.next() else {
        return ExitCode::SUCCESS;
    };
    if command == "doctor" {
        let mut args = args.peekable();
        if PrerequisitesArgs::requested(args.peek()) {
            return match PrerequisitesArgs::parse(args) {
                Ok(_) => report(
                    &"doctor --repository: the live gh transport it reads through is not built yet (#273)",
                ),
                Err(usage) => report(&usage),
            };
        }
        return run_doctor(args);
    }
    if command == "check" {
        return run_check(args);
    }
    if command == "pre-push" {
        return run_pre_push(args);
    }
    if command == "open" {
        return run_open(args);
    }
    if command == "status" {
        return match StatusCommand::parse(args) {
            Ok(StatusCommand::Local(format)) => run_local_status(format),
            Ok(StatusCommand::Pull(args)) => run_pull_status(&args),
            Err(usage) => report(&usage),
        };
    }
    if command == "reconcile" {
        return match reconcile::prepare(args, env::var_os(reconcile::RUN_ATTEMPT)) {
            Ok(sweep) => report(&reconcile::ReconcileRefused::NotBuilt(sweep)),
            Err(refused) => report(&refused),
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

/// `assay open` (#54): exit 2 for a re-run or a bad environment, and, until the live
/// transport (#273) exists, for every run, naming it.
fn run_open(args: impl Iterator<Item = std::ffi::OsString>) -> ExitCode {
    let variables = OpenVariables {
        run_id: env::var_os("GITHUB_RUN_ID"),
        run_attempt: env::var_os("GITHUB_RUN_ATTEMPT"),
        repository_id: env::var_os("GITHUB_REPOSITORY_ID"),
    };
    match OpenCommand::parse(args, variables) {
        Ok(_) => report(&"open: the GitHub transport it writes through is not built yet (#273)"),
        Err(usage) => report(&usage),
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
        tree: tree_variables(),
        vars: env::vars_os().collect(),
    };
    let checked = match check::run(args, variables) {
        Ok(checked) => checked,
        Err(error) => return report(&error),
    };
    let written = match checked.report_to {
        check::ReportTo::Stdout => checked.write(io::stdout().lock()),
        check::ReportTo::Stderr => checked.write(io::stderr().lock()),
    };
    if let Err(error) = written {
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

/// `assay pre-push`: exit 0 when no Weigh-In item blocks the push (pending ones only warn, and
/// a CP1 that did not run says so), 1 when one does, 2 when CP1 cannot run.
fn run_pre_push(args: impl Iterator<Item = std::ffi::OsString>) -> ExitCode {
    let variables = check::CheckVariables {
        tree: tree_variables(),
        vars: env::vars_os().collect(),
    };
    let weigh_in = match check::pre_push(args, variables) {
        Ok(weigh_in) => weigh_in,
        Err(error) => return report(&error),
    };
    let mut out = io::stdout().lock();
    let written = out
        .write_all(weigh_in.render().as_bytes())
        .and_then(|()| out.flush());
    if let Err(error) = written {
        return report(&error);
    }
    if weigh_in.blocks_push() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// `assay status --local`: exit 0 when the status is written, 2 when it cannot be.
fn run_local_status(format: Format) -> ExitCode {
    let shown = match status_local::run(tree_variables()) {
        Ok(shown) => shown,
        Err(error) => return report(&error),
    };
    let bytes = match format {
        Format::Json => shown.report.json(),
        Format::Text => shown.report.text().as_bytes(),
    };
    let mut out = io::stdout().lock();
    if let Err(error) = out.write_all(bytes).and_then(|()| out.flush()) {
        return report(&error);
    }
    match &shown.scratch_left {
        Some(left) => report(left),
        None => ExitCode::SUCCESS,
    }
}

/// `assay status [--pr <n>]`: exit 0 when the status is written, 2 when it cannot be or when
/// a scratch directory is left behind.
fn run_pull_status(args: &StatusArgs) -> ExitCode {
    let variables = status_pr::PullVariables {
        tree: tree_variables(),
        token: env::var_os(status_pr::TOKEN),
        app_id: env::var_os(status_pr::APP_ID),
        bot_id: env::var_os(status_pr::BOT_ID),
        gh: GhEnvironment::from_process_env(),
    };
    let ended = match status_pr::run(args, variables) {
        Ok(ended) => ended,
        Err(error) => return report(&error),
    };
    let mut code = match ended.status {
        Ok(bytes) => {
            let mut out = io::stdout().lock();
            match out.write_all(&bytes).and_then(|()| out.flush()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => report(&error),
            }
        }
        Err(error) => report(&error),
    };
    if let Some(left) = &ended.gh_left {
        code = report(left);
    }
    if let Some(left) = &ended.scratch_left {
        code = report(left);
    }
    code
}

/// The environment that opens the working tree (`check --local`, `status --local`).
fn tree_variables() -> check::TreeVariables {
    check::TreeVariables {
        cwd: env::current_dir(),
        path: env::var_os("PATH"),
        home: env::var_os("HOME"),
        xdg_config_home: env::var_os("XDG_CONFIG_HOME"),
    }
}

/// Writes `error` to stderr and returns the error exit code.
fn report(error: &dyn std::fmt::Display) -> ExitCode {
    // stderr is the last channel: a failed write there leaves the exit code as the report.
    match writeln!(io::stderr().lock(), "assay: {error}") {
        Ok(()) | Err(_) => ExitCode::from(ERROR),
    }
}
