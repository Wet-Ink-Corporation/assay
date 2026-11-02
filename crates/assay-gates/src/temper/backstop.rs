//! The forbid backstop (#172, `docs/architecture/03-contracts.md` §2.1 step 5a): one clippy
//! build of the head tree in `gates-lint`, with base's forbidden lints ([`ForbidSet`]) as `-F`
//! through `CARGO_ENCODED_RUSTFLAGS` and no lint cap, so rustc refuses every attribute that
//! would lower one (E0453), however a macro or proc-macro assembled it.
//!
//! temper's own `baseline --check` cannot carry the flags: it runs clippy with `--cap-lints
//! warn`, under which a forbid is capped and the `allow` wins. The backstop runs after the
//! declared steps and before `baseline --check`, so no build script or proc-macro has run in the
//! tree before it.
//!
//! What the PR's `.cargo/config.toml` could do to this run is overridden: `cargo-clippy` runs
//! directly, so an `[alias] clippy` cannot shadow it; `CARGO_ENCODED_RUSTFLAGS` takes precedence
//! over every `rustflags` source of config; `RUSTC_WRAPPER` is set empty and `RUSTC` to `rustc`
//! (a wrapper could strip the flags, and the artifact it left would later count as fresh);
//! `CARGO_TARGET_DIR` is the head tree's `target`; and `--config` sets `[env] CLIPPY_ARGS`
//! empty, which clippy-driver would otherwise append (`--cap-lints allow`).
//!
//! The decision is the run's exit code: E0453 is a hard error, so cargo exits non-zero, and any
//! other failure fails the gate too (a crate that does not build hides its dependents). The JSON
//! it prints only locates findings.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::num::NonZeroU64;
use std::path::Path;
use std::time::Duration;

use assay_app::Termination;
use serde::Deserialize;

use super::forbid::ForbidSet;
use super::lint_findings::MAX_REPORTED;
use super::steps::capture_run;
use super::{ErrorOf, TemperAdapter};
use crate::capture::CaptureSupervisor;
use crate::revision::RevisionFiles;

/// The program, looked up on the gate environment's `PATH`: clippy's cargo subcommand, run
/// directly so that no cargo alias resolves first.
const PROGRAM: &str = "cargo-clippy";

/// How the backstop is named in errors.
pub(super) const PROCESS: &str = "the forbid backstop";

/// The error code of an attribute that lowers a forbidden lint.
const OVERRULED: &str = "E0453";

/// What the backstop found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Backstop {
    /// Base forbids nothing, so nothing ran.
    NotRun,
    /// The build exited 0: no forbidden lint is lowered.
    Passed,
    /// The build exited `code`, not 0; `overrides` are the E0453s it located.
    Failed {
        /// Its exit code.
        code: i32,
        /// Each located E0453, at most [`MAX_REPORTED`], in file and line order.
        overrides: Vec<Override>,
    },
    /// The build did not exit (timeout, signal, output cap).
    Stopped(Termination),
}

/// An E0453 at a file of the head tree and a line.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Override {
    pub(super) file: String,
    pub(super) line: NonZeroU64,
}

impl<S: CaptureSupervisor, R: RevisionFiles> TemperAdapter<S, R> {
    /// Runs the backstop for `forbidden` from the head tree under `timeout`, with every feature
    /// when `all_features`; nothing when `forbidden` is empty.
    pub(super) fn backstop(
        &mut self,
        forbidden: &ForbidSet,
        all_features: bool,
        timeout: Duration,
    ) -> Result<Backstop, ErrorOf<S, R>> {
        if forbidden.is_empty() {
            return Ok(Backstop::NotRun);
        }
        let head_tree = &self.settings.steps.head_tree;
        let args = arguments(all_features);
        // The spec takes one slice: a copy of the step environment, then the backstop's own.
        let mut env = self.settings.steps.env.clone();
        env.extend(environment(forbidden, head_tree));
        let run = capture_run(
            &mut self.supervisor,
            (PROCESS, Path::new(PROGRAM), &args),
            (head_tree, &env),
            timeout,
        )?;
        Ok(match run.termination {
            Termination::Exited(0) => Backstop::Passed,
            Termination::Exited(code) => Backstop::Failed {
                code,
                overrides: overrides(&run.stdout),
            },
            stopped => Backstop::Stopped(stopped),
        })
    }
}

/// The arguments after the program.
fn arguments(all_features: bool) -> Vec<String> {
    let head = ["clippy", "--workspace", "--all-targets"];
    let features = all_features.then_some("--all-features");
    let tail = [
        "--locked",
        "--keep-going",
        "--message-format=json-diagnostic-short",
        "--config",
        "env.CLIPPY_ARGS.value=\"\"",
        "--config",
        "env.CLIPPY_ARGS.force=true",
    ];
    head.into_iter()
        .chain(features)
        .chain(tail)
        .map(str::to_owned)
        .collect()
}

/// The variables the backstop sets after the step environment, so they win over anything
/// there (none of them passes the gate environment's allowlist).
fn environment(forbidden: &ForbidSet, head_tree: &Path) -> [(OsString, OsString); 5] {
    [
        ("CARGO_ENCODED_RUSTFLAGS".into(), forbidden.encoded().into()),
        ("RUSTC_WRAPPER".into(), OsString::new()),
        ("RUSTC".into(), "rustc".into()),
        ("CARGO_TARGET_DIR".into(), head_tree.join("target").into()),
        ("CARGO_TERM_COLOR".into(), "never".into()),
    ]
}

/// One line of cargo's JSON output; only compiler messages carry a diagnostic.
#[derive(Debug, Deserialize)]
struct MessageLine {
    reason: String,
    #[serde(default)]
    message: Option<Diagnostic>,
}

#[derive(Debug, Deserialize)]
struct Diagnostic {
    #[serde(default)]
    code: Option<Code>,
    #[serde(default)]
    spans: Vec<Span>,
}

#[derive(Debug, Deserialize)]
struct Code {
    code: String,
}

#[derive(Debug, Deserialize)]
struct Span {
    file_name: String,
    line_start: u64,
    is_primary: bool,
}

/// The E0453s `stdout` locates: each primary span of a compiler message with that code, at a
/// plain relative file and a line above 0, once each, at most [`MAX_REPORTED`].
///
/// The output comes from a build the PR controls, so it never decides anything: a line that is
/// not such a message (any other JSON, or text a proc-macro printed) locates nothing, and the
/// failure it would explain is still reported (`forbid-backstop`).
pub(super) fn overrides(stdout: &[u8]) -> Vec<Override> {
    let mut found = BTreeSet::new();
    let quoted = format!("\"{OVERRULED}\"");
    for line in stdout.split(|byte| *byte == b'\n') {
        if found.len() >= MAX_REPORTED {
            break;
        }
        if !line
            .windows(quoted.len())
            .any(|window| window == quoted.as_bytes())
        {
            continue;
        }
        let Ok(message) = serde_json::from_slice::<MessageLine>(line) else {
            continue;
        };
        let Some(diagnostic) = message
            .message
            .filter(|_| message.reason == "compiler-message")
        else {
            continue;
        };
        if diagnostic.code.is_none_or(|code| code.code != OVERRULED) {
            continue;
        }
        let primary = diagnostic
            .spans
            .into_iter()
            .filter(|span| span.is_primary && is_plain_relative(&span.file_name));
        for span in primary {
            if let Some(line) = NonZeroU64::new(span.line_start) {
                found.insert(Override {
                    file: span.file_name,
                    line,
                });
            }
        }
    }
    found.into_iter().take(MAX_REPORTED).collect()
}

/// Whether `file` is a relative path of plain components (no empty, `.` or `..` one), as a
/// file of the head tree is named.
fn is_plain_relative(file: &str) -> bool {
    !file.is_empty()
        && file
            .split('/')
            .all(|component| !matches!(component, "" | "." | ".."))
        && !file.contains('\\')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A compiler message as cargo prints it, with one span per `(file, line, primary)`.
    fn message(code: Option<&str>, spans: &[(&str, u64, bool)]) -> String {
        let spans: Vec<serde_json::Value> = spans
            .iter()
            .map(|(file, line, primary)| {
                serde_json::json!({
                    "file_name": file, "byte_start": 0, "byte_end": 1, "line_start": line,
                    "line_end": line, "column_start": 1, "column_end": 2,
                    "is_primary": primary, "text": [], "label": null,
                    "suggested_replacement": null, "suggestion_applicability": null,
                    "expansion": null,
                })
            })
            .collect();
        serde_json::json!({
            "reason": "compiler-message",
            "package_id": "path+file:///head#fx@0.1.0",
            "manifest_path": "/head/Cargo.toml",
            "target": { "name": "fx" },
            "message": {
                "rendered": "error[E0453]: allow(unsafe_code) incompatible with previous forbid",
                "$message_type": "diagnostic",
                "children": [],
                "code": code.map(|code| serde_json::json!({ "code": code, "explanation": "" })),
                "level": "error",
                "message": "allow(unsafe_code) incompatible with previous forbid",
                "spans": spans,
            },
        })
        .to_string()
    }

    fn at(file: &str, line: u64) -> Override {
        Override {
            file: file.to_owned(),
            line: NonZeroU64::new(line).expect("a test line is above 0"),
        }
    }

    #[test]
    fn locates_each_primary_span_of_an_e0453_once_in_file_and_line_order() {
        let stdout = [
            r#"{"reason":"compiler-artifact","package_id":"x"}"#.to_owned(),
            message(
                Some("E0453"),
                &[("src/lib.rs", 5, true), ("src/m.rs", 9, false)],
            ),
            message(Some("E0453"), &[("crates/a/src/lib.rs", 3, true)]),
            message(Some("E0453"), &[("src/lib.rs", 5, true)]),
            r#"{"reason":"build-finished","success":false}"#.to_owned(),
        ]
        .join("\n");

        let found = overrides(stdout.as_bytes());

        assert_eq!(found, [at("crates/a/src/lib.rs", 3), at("src/lib.rs", 5)]);
    }

    #[test]
    fn other_codes_lines_and_files_locate_nothing() {
        let stdout = [
            message(Some("E0308"), &[("src/lib.rs", 1, true)]),
            message(None, &[("src/lib.rs", 2, true)]),
            "not JSON, but it mentions \"E0453\"".to_owned(),
            r#"{"reason":"build-script-executed","message":"\"E0453\""}"#.to_owned(),
            message(
                Some("E0453"),
                &[("/root/.cargo/registry/src/x/lib.rs", 3, true)],
            ),
            message(Some("E0453"), &[("../outside.rs", 4, true)]),
            message(Some("E0453"), &[("src/./lib.rs", 5, true)]),
            message(Some("E0453"), &[("src//lib.rs", 6, true)]),
            message(Some("E0453"), &[("src\\lib.rs", 7, true)]),
            message(Some("E0453"), &[("", 8, true)]),
            message(Some("E0453"), &[("src/lib.rs", 0, true)]),
            message(Some("E0453"), &[("src/lib.rs", 9, false)]),
        ]
        .join("\n");

        let found = overrides(stdout.as_bytes());

        assert_eq!(found, []);
    }

    #[test]
    fn keeps_exactly_the_most_findings_and_no_more() {
        let lines: Vec<String> = (1..=1_001)
            .map(|line| message(Some("E0453"), &[("src/lib.rs", line, true)]))
            .collect();

        let found = overrides(lines.join("\n").as_bytes());

        assert_eq!(found.len(), MAX_REPORTED);
        assert_eq!(found.last(), Some(&at("src/lib.rs", 1_000)));
    }

    #[test]
    fn runs_clippy_directly_over_the_workspace_with_every_target_and_no_cap() {
        let with = arguments(true);
        let without = arguments(false);

        assert_eq!(
            with,
            [
                "clippy",
                "--workspace",
                "--all-targets",
                "--all-features",
                "--locked",
                "--keep-going",
                "--message-format=json-diagnostic-short",
                "--config",
                "env.CLIPPY_ARGS.value=\"\"",
                "--config",
                "env.CLIPPY_ARGS.force=true",
            ]
        );
        assert_eq!(
            without.iter().collect::<Vec<_>>(),
            with.iter()
                .filter(|arg| *arg != "--all-features")
                .collect::<Vec<_>>()
        );
        assert!(
            !with.iter().any(|arg| arg.contains("cap-lints")),
            "nothing caps the lints"
        );
    }
}
