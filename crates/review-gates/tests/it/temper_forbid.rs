//! The forbid backstop in `gates-lint` (#172, `docs/architecture/03-contracts.md` §2.1 step 5a):
//! base's deny and forbid `[workspace.lints]`, minus admitted exceptions, passed as `-F` through
//! `CARGO_ENCODED_RUSTFLAGS` to an uncapped `cargo-clippy` run of the head tree, between the
//! declared steps and `baseline --check`.
//!
//! The scripted supervisor of `temper_lint` plays it by its first argument, `clippy`; the last
//! test runs the real `cargo-clippy` with the adapter's own spec over a fixture workspace whose
//! overrides a `macro_rules!` and a proc-macro assemble, behind a hostile `.cargo/config.toml`.

use std::assert_matches;
use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use review_app::{GateError, GateJobOutcome, Termination};
use review_domain::{Severity, Side};
use review_gates::{
    CaptureSupervisor, CapturedRun, GateEnvironment, ProcessSpec, Revision, TemperError,
    TemperSettings,
};

use crate::support::SeenSpec;
use crate::temper_lint::{
    DIFF_TOTALS, FAILED, FakeError, FakeLint, PASSED, outcome, outcome_with, run, shown,
};
use crate::temper_support::{CONFIG, EXCEPTIONS, Pr};

/// Base's root manifest: two forbidden lints, a denied group, a warn and an allow.
const BASE_MANIFEST: &str = r#"[workspace]
members = ["fx", "mac"]

[workspace.lints.rust]
unsafe_code = "forbid"
missing_docs = "warn"

[workspace.lints.clippy]
all = { level = "deny", priority = -1 }
unwrap_used = "deny"
naive_bytecount = "allow"
"#;

/// What [`BASE_MANIFEST`] forbids, as `CARGO_ENCODED_RUSTFLAGS`.
const FORBIDDEN: &str = "-Fclippy::unwrap_used\u{1f}-Funsafe_code";

/// A PR whose base has [`BASE_MANIFEST`].
fn pr() -> Result<Pr, Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(Revision::Base, "Cargo.toml", BASE_MANIFEST);
    Ok(pr)
}

/// The backstop's run, by its first argument.
fn backstop(lint: &FakeLint) -> Result<&SeenSpec, Box<dyn Error>> {
    lint.seen
        .iter()
        .find(|seen| seen.args.first().map(String::as_str) == Some("clippy"))
        .ok_or_else(|| "the backstop did not run".into())
}

fn os(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    pairs
        .iter()
        .map(|(name, value)| (OsString::from(name), OsString::from(value)))
        .collect()
}

#[test]
fn runs_clippy_with_base_forbidden_lints_between_the_steps_and_baseline_check()
-> Result<(), Box<dyn Error>> {
    let pr = pr()?;
    let mut lint = FakeLint::leaving(DIFF_TOTALS);

    let (outcome, _) = outcome(&pr, &mut lint)?;

    assert_eq!(outcome, PASSED);
    assert_eq!(lint.ran(), ["fmt", "deny", "clippy", "clippy", "baseline"]);
    let passes = backstops(&lint);
    let parent = pr
        .head_tree
        .parent()
        .ok_or("the head tree has a parent")?
        .to_string_lossy();
    let (target, _) = lint
        .backstop_targets
        .first()
        .ok_or("the backstop saw its target")?;
    // Each pass pins `debug_assertions` for its profiles, whatever the manifest sets.
    let debug = pass_args(&["clippy"], [("dev", true), ("test", true)]);
    let release = pass_args(
        &["clippy", "--release"],
        [("release", false), ("bench", false)],
    );
    assert_eq!(
        passes.iter().map(|seen| &seen.args).collect::<Vec<_>>(),
        [&debug, &release],
        "a debug pass, then a release pass"
    );
    for seen in passes {
        assert_eq!(seen.program, Path::new("cargo-clippy"));
        assert_eq!(seen.cwd, pr.head_tree);
        assert_eq!(
            seen.timeout,
            Duration::from_mins(10),
            "half the plugin's timeout_s each"
        );
        assert_eq!(seen.max_output_bytes, 16 << 20);
        assert_eq!(
            seen.env,
            os(&[
                ("PATH", "/usr/bin:/bin"),
                ("GIT_CONFIG_NOSYSTEM", "1"),
                ("GIT_CONFIG_GLOBAL", "/dev/null"),
                ("GIT_CEILING_DIRECTORIES", &parent),
                ("CARGO_ENCODED_RUSTFLAGS", FORBIDDEN),
                ("RUSTC_WRAPPER", ""),
                ("RUSTC", "rustc"),
                ("CARGO_TARGET_DIR", &target.to_string_lossy()),
                // Cargo keeps fingerprints in `build.build-dir`, which config could point at
                // committed artifacts.
                ("CARGO_BUILD_BUILD_DIR", &target.to_string_lossy()),
                ("CARGO_TERM_COLOR", "never"),
            ])
        );
    }
    Ok(())
}

/// The argv of a pass that starts with `head`, with every feature, and pins debug assertions
/// to each (profile, value) of `pins`.
fn pass_args(head: &[&str], pins: [(&str, bool); 2]) -> Vec<String> {
    let tail = [
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
    ];
    let pinned = pins.into_iter().flat_map(|(profile, value)| {
        [
            "--config".to_owned(),
            format!("profile.{profile}.debug-assertions={value}"),
        ]
    });
    head.iter()
        .chain(&tail)
        .map(|arg| (*arg).to_owned())
        .chain(pinned)
        .collect()
}

/// Every backstop pass, in order.
fn backstops(lint: &FakeLint) -> Vec<&SeenSpec> {
    lint.seen
        .iter()
        .filter(|seen| seen.args.first().map(String::as_str) == Some("clippy"))
        .collect()
}

/// A `target/` the PR commits (#348 accepts an untracked root `target/`, and a tracked one is
/// no trust path) could hold fresh fingerprints and metadata, so cargo would never compile the
/// PR's sources (#172 F2). The backstop builds into a directory of its own, created empty
/// beside the head tree for this run and removed after it.
#[test]
fn builds_into_a_fresh_directory_outside_the_head_tree_and_removes_it() -> Result<(), Box<dyn Error>>
{
    let pr = pr()?;
    let parent = pr.head_tree.parent().ok_or("the head tree has a parent")?;
    let mut lint = FakeLint::leaving(DIFF_TOTALS);

    outcome(&pr, &mut lint)?;

    let targets = &lint.backstop_targets;
    let [(debug, fresh), (release, _)] = targets.as_slice() else {
        return Err(format!("two passes, got {targets:?}").into());
    };
    assert!(
        *fresh,
        "{debug:?} was an empty directory when the build started"
    );
    assert_eq!(debug, release, "both passes share it");
    assert!(!debug.starts_with(&pr.head_tree), "{debug:?}");
    assert_eq!(debug.parent(), Some(parent), "beside the head tree");
    assert!(!debug.exists(), "{debug:?} is removed after the run");
    Ok(())
}

#[test]
fn heads_lints_table_does_not_shrink_what_is_forbidden() -> Result<(), Box<dyn Error>> {
    let mut pr = pr()?;
    pr.set(
        Revision::Head,
        "Cargo.toml",
        "[workspace]\nmembers = []\n\n[workspace.lints.rust]\nunsafe_code = \"allow\"\n",
    );
    let mut lint = FakeLint::leaving(DIFF_TOTALS);

    outcome(&pr, &mut lint)?;

    let env = &backstop(&lint)?.env;
    assert!(
        env.contains(&(
            OsString::from("CARGO_ENCODED_RUSTFLAGS"),
            OsString::from(FORBIDDEN)
        )),
        "{env:?}"
    );
    Ok(())
}

#[test]
fn an_admitted_exception_lifts_its_lint() -> Result<(), Box<dyn Error>> {
    let mut pr = pr()?;
    pr.set(
        Revision::Base,
        EXCEPTIONS,
        "[[exception]]\npath = \"src/lib.rs\"\nlint = \"clippy::unwrap_used\"\nreason = \"r\"\n",
    );
    let mut lint = FakeLint::leaving(DIFF_TOTALS);

    outcome(&pr, &mut lint)?;

    let env = &backstop(&lint)?.env;
    assert!(
        env.contains(&(
            OsString::from("CARGO_ENCODED_RUSTFLAGS"),
            OsString::from("-Funsafe_code")
        )),
        "{env:?}"
    );
    Ok(())
}

#[test]
fn nothing_runs_for_a_base_that_forbids_nothing() -> Result<(), Box<dyn Error>> {
    let mut warn_only = pr()?;
    warn_only.set(
        Revision::Base,
        "Cargo.toml",
        "[workspace]\nmembers = []\n\n[workspace.lints.clippy]\nall = { level = \"deny\", \
         priority = -1 }\npedantic = \"warn\"\n",
    );
    let no_manifest = Pr::new()?;
    for pr in [warn_only, no_manifest] {
        let mut lint = FakeLint::leaving(DIFF_TOTALS);

        let (outcome, _) = outcome(&pr, &mut lint)?;

        assert_eq!(outcome, PASSED);
        assert_eq!(lint.ran(), ["fmt", "deny", "baseline"]);
    }
    Ok(())
}

#[test]
fn base_without_all_features_runs_without_them() -> Result<(), Box<dyn Error>> {
    let mut pr = pr()?;
    pr.set(
        Revision::Base,
        CONFIG,
        r#"{"gate": {"allFeatures": false}}"#,
    );
    let mut lint = FakeLint::leaving(DIFF_TOTALS);

    outcome(&pr, &mut lint)?;

    let args = &backstop(&lint)?.args;
    assert!(!args.iter().any(|arg| arg == "--all-features"), "{args:?}");
    Ok(())
}

/// Head's `.claude/temper.json` and `.assay/exceptions.toml` are the PR's: neither turns
/// features off nor lifts a lint (normal review F2).
#[test]
fn heads_config_and_exceptions_change_neither_features_nor_the_forbidden_lints()
-> Result<(), Box<dyn Error>> {
    let mut pr = pr()?;
    pr.set(
        Revision::Head,
        CONFIG,
        r#"{"gate": {"allFeatures": false}}"#,
    );
    pr.set(
        Revision::Head,
        EXCEPTIONS,
        "[[exception]]\npath = \"src/lib.rs\"\nlint = \"unsafe_code\"\nreason = \"r\"\n",
    );
    let mut lint = FakeLint::leaving(DIFF_TOTALS);

    outcome(&pr, &mut lint)?;

    for seen in backstops(&lint) {
        assert!(
            seen.args.iter().any(|arg| arg == "--all-features"),
            "{:?}",
            seen.args
        );
        assert!(
            seen.env.contains(&(
                OsString::from("CARGO_ENCODED_RUSTFLAGS"),
                OsString::from(FORBIDDEN)
            )),
            "{:?}",
            seen.env
        );
    }
    assert_eq!(backstops(&lint).len(), 2, "both passes ran");
    Ok(())
}

/// `debug_assertions` is on in one profile and off in the other, so code under either
/// `cfg(debug_assertions)` or its negation is seen by one pass (#172 F1).
#[test]
fn a_failing_release_pass_fails_the_gate_and_a_failing_debug_pass_skips_it()
-> Result<(), Box<dyn Error>> {
    let pr = pr()?;
    let failed = Termination::Exited(101);
    let cases = [
        (
            [Termination::Exited(0), failed],
            vec!["fmt", "deny", "clippy", "clippy", "baseline"],
        ),
        (
            [failed, Termination::Exited(0)],
            vec!["fmt", "deny", "clippy", "baseline"],
        ),
    ];
    for (passes, ran) in cases {
        let mut lint = FakeLint::leaving(DIFF_TOTALS).step_attempts("clippy", &passes);

        let (outcome, document) = outcome(&pr, &mut lint)?;

        assert_eq!(outcome, FAILED, "{passes:?}");
        assert_eq!(lint.ran(), ran, "{passes:?}");
        let steps = document.and_then(|document| document.failed_steps);
        assert_eq!(
            steps,
            Some(vec!["forbid-backstop".to_owned()]),
            "{passes:?}"
        );
    }
    Ok(())
}

#[test]
fn a_release_pass_that_does_not_exit_is_that_gate_error() -> Result<(), Box<dyn Error>> {
    let pr = pr()?;
    let mut lint = FakeLint::leaving(DIFF_TOTALS)
        .step_attempts("clippy", &[Termination::Exited(0), Termination::TimedOut]);

    let (outcome, _) = outcome(&pr, &mut lint)?;

    assert_eq!(outcome, GateJobOutcome::Error(GateError::Timeout));
    assert_eq!(lint.ran(), ["fmt", "deny", "clippy", "clippy"]);
    let removed = lint
        .backstop_targets
        .iter()
        .all(|(target, _)| !target.exists());
    assert!(removed, "{:?} is removed", lint.backstop_targets);
    Ok(())
}

/// One E0453 as cargo prints it, at `file:line`.
fn overruled(file: &str, line: u64) -> String {
    serde_json::json!({
        "reason": "compiler-message",
        "package_id": "path+file:///head/fx#0.1.0",
        "message": {
            "rendered": "error[E0453]: allow(unsafe_code) incompatible with previous forbid",
            "code": { "code": "E0453", "explanation": "" },
            "level": "error",
            "message": "allow(unsafe_code) incompatible with previous forbid",
            "spans": [{ "file_name": file, "line_start": line, "line_end": line,
                        "is_primary": true }],
            "children": [],
        },
    })
    .to_string()
}

#[test]
fn an_overridden_forbidden_lint_fails_the_gate_at_its_line() -> Result<(), Box<dyn Error>> {
    let mut pr = pr()?;
    pr.set(
        Revision::Head,
        "src/lib.rs",
        "//! x\nhole!(unsafe_code);\nfn f() {}\n",
    );
    let mut lint = FakeLint::leaving(DIFF_TOTALS).step_ends("clippy", Termination::Exited(101));
    lint.backstop_stdout = format!(
        "{}\n{}\n{{\"reason\":\"build-finished\",\"success\":false}}\n",
        overruled("src/lib.rs", 2),
        overruled("src/lib.rs", 2),
    )
    .into_bytes();

    let (outcome, document) = outcome(&pr, &mut lint)?;

    assert_eq!(outcome, FAILED);
    assert_eq!(
        shown(document),
        [(
            "forbid-override".to_owned(),
            "src/lib.rs".to_owned(),
            2,
            Some(Side::Right),
            Severity::Blocker,
            "hole!(unsafe_code);".to_owned(),
        )]
    );
    assert_eq!(lint.ran(), ["fmt", "deny", "clippy", "baseline"]);
    Ok(())
}

#[test]
fn a_failed_backstop_without_a_located_override_still_fails() -> Result<(), Box<dyn Error>> {
    let pr = pr()?;
    let mut lint = FakeLint::leaving(DIFF_TOTALS).step_ends("clippy", Termination::Exited(101));
    lint.backstop_stdout = b"a proc-macro printed this\n".to_vec();

    let (outcome, document) = outcome(&pr, &mut lint)?;

    assert_eq!(outcome, FAILED);
    let found = shown(document);
    assert_eq!(
        found
            .iter()
            .map(|(rule, file, line, _, severity, _)| (
                rule.as_str(),
                file.as_str(),
                *line,
                *severity
            ))
            .collect::<Vec<_>>(),
        [("forbid-backstop", "Cargo.toml", 1, Severity::Blocker)]
    );
    Ok(())
}

#[test]
fn a_backstop_that_does_not_exit_is_that_gate_error() -> Result<(), Box<dyn Error>> {
    let pr = pr()?;
    let cases = [
        (Termination::TimedOut, GateError::Timeout),
        (Termination::Signalled, GateError::Signal),
        (Termination::OutputTooLarge, GateError::OutputTooLarge),
    ];
    for (termination, expected) in cases {
        let mut lint = FakeLint::leaving(DIFF_TOTALS).step_ends("clippy", termination);

        let (outcome, _) = outcome(&pr, &mut lint)?;

        assert_eq!(outcome, GateJobOutcome::Error(expected), "{termination:?}");
        assert_eq!(
            lint.ran(),
            ["fmt", "deny", "clippy"],
            "{termination:?}: run once, then nothing"
        );
    }
    Ok(())
}

#[test]
fn a_backstop_that_cannot_start_is_an_error() -> Result<(), Box<dyn Error>> {
    let pr = pr()?;
    let mut lint = FakeLint::leaving(DIFF_TOTALS);
    lint.cannot_start = Some("clippy".to_owned());

    let ran = run(&pr, &mut lint)?;

    assert_matches!(ran, Err(TemperError::Lint { ref process, .. }) if process == "the forbid backstop");
    assert_eq!(
        backstop_dirs(&pr)?,
        Vec::<OsString>::new(),
        "the target directory is removed on the error path too"
    );
    Ok(())
}

/// The `assay-backstop-*` directories left beside `pr`'s head tree.
fn backstop_dirs(pr: &Pr) -> Result<Vec<OsString>, Box<dyn Error>> {
    let parent = pr.head_tree.parent().ok_or("the head tree has a parent")?;
    Ok(fs::read_dir(parent)?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .filter(|name| name.to_string_lossy().starts_with("assay-backstop-"))
        .collect())
}

#[test]
fn a_base_lints_table_or_exception_it_cannot_read_is_refused_before_anything_runs()
-> Result<(), Box<dyn Error>> {
    let cases = [
        (
            "Cargo.toml",
            "[workspace.lints.rust]\n\"unsafe_code -A\" = \"forbid\"\n",
            "Cargo.toml",
        ),
        (
            "Cargo.toml",
            "[workspace.lints.rust]\nx = \"loud\"\n",
            "Cargo.toml",
        ),
        (
            EXCEPTIONS,
            "[[exception]]\npath = \"a.rs\"\nlint = \"unsafe code\"\nreason = \"r\"\n",
            EXCEPTIONS,
        ),
    ];
    for (path, text, refused) in cases {
        let mut pr = pr()?;
        pr.set(Revision::Base, path, text);
        let mut lint = FakeLint::leaving(DIFF_TOTALS);

        let ran = run(&pr, &mut lint)?;

        assert_matches!(
            ran,
            Err(TemperError::Malformed { revision: Revision::Base, path, .. }) if path == refused,
            "{text}"
        );
        assert_eq!(lint.ran(), Vec::<&str>::new(), "{text}: nothing ran");
    }
    Ok(())
}

/// Plays the steps and `baseline --check` as [`FakeLint`] does, but runs the backstop for real:
/// exactly the adapter's program, arguments, working directory and environment.
struct RealBackstop<'l> {
    fake: &'l mut FakeLint,
    /// The real run's stderr, for a failing assertion's message.
    stderr: String,
}

impl CaptureSupervisor for RealBackstop<'_> {
    type Error = FakeError;

    fn capture(&mut self, spec: &ProcessSpec<'_>) -> Result<CapturedRun, FakeError> {
        if spec.program != Path::new("cargo-clippy") {
            return self.fake.capture(spec);
        }
        let output = Command::new(spec.program)
            .args(spec.args)
            .current_dir(spec.cwd)
            .env_clear()
            .envs(spec.env.iter().map(|(name, value)| (name, value)))
            .output()
            .map_err(|error| FakeError(error.to_string()))?;
        self.stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let termination = output
            .status
            .code()
            .map_or(Termination::Signalled, Termination::Exited);
        Ok(CapturedRun {
            termination,
            stdout: output.stdout,
        })
    }
}

/// The fixture workspace: `fx` overrides `unsafe_code` through a `macro_rules!` (line 9) and
/// `clippy::unwrap_used` through the proc-macro `mac` (line 10), and a `.cargo/config.toml` tries
/// every config route around the flags.
const FIXTURE: [(&str, &str); 7] = [
    (
        "Cargo.toml",
        "[workspace]\nmembers = [\"fx\", \"mac\"]\nresolver = \"3\"\n",
    ),
    (
        "Cargo.lock",
        "# This file is automatically @generated by Cargo.\n# It is not intended for manual \
         editing.\nversion = 4\n\n[[package]]\nname = \"fx\"\nversion = \"0.1.0\"\ndependencies \
         = [\n \"mac\",\n]\n\n[[package]]\nname = \"mac\"\nversion = \"0.1.0\"\n",
    ),
    (
        "fx/Cargo.toml",
        "[package]\nname = \"fx\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\n\
         mac = { path = \"../mac\" }\n",
    ),
    (
        "fx/src/lib.rs",
        "//! Lowers two forbidden lints where no attribute is written out.\n\
         \n\
         macro_rules! hole {\n    ($lint:path) => {\n        #[allow($lint)]\n        \
         pub fn from_macro_rules() {}\n    };\n}\n\
         hole!(unsafe_code);\n\
         mac::hole!(clippy::unwrap_used);\n",
    ),
    (
        "mac/Cargo.toml",
        "[package]\nname = \"mac\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[lib]\n\
         proc-macro = true\n",
    ),
    (
        "mac/src/lib.rs",
        "//! Emits an item that allows the lint it is given.\n\
         use proc_macro::TokenStream;\n\
         /// `#[allow(<lint>)] pub fn from_proc_macro() {}`.\n\
         #[proc_macro]\n\
         pub fn hole(lint: TokenStream) -> TokenStream {\n    \
         format!(\"#[{}({lint})] pub fn from_proc_macro() {{}}\", \"allow\")\n        \
         .parse()\n        .unwrap_or_default()\n}\n",
    ),
    (
        ".cargo/config.toml",
        "[alias]\nclippy = \"check\"\n\n[build]\nrustc-wrapper = \"/bin/false\"\n\
         rustc = \"/bin/false\"\ntarget-dir = \"elsewhere\"\nrustflags = [\"-Aunsafe_code\"]\n\n\
         [target.'cfg(all())']\nrustflags = [\"-Aunsafe_code\"]\n\n[env]\n\
         CLIPPY_ARGS = { value = \"--cap-lints__CLIPPY_HACKERY__allow__CLIPPY_HACKERY__\", \
         force = true }\n",
    ),
];

/// A PR whose head is [`FIXTURE`] on disk and in the tree, with `fx/src/lib.rs` replaced by
/// `lib` when given, and the workspace's own toolchain (whatever rustup's default is).
fn fixture(lib: Option<&str>) -> Result<Pr, Box<dyn Error>> {
    fixture_with(
        &lib.map(|lib| ("fx/src/lib.rs", lib))
            .into_iter()
            .collect::<Vec<_>>(),
    )
}

/// [`FIXTURE`] with each of `replaced`, by path, on disk and in the head tree.
fn fixture_with(replaced: &[(&str, &str)]) -> Result<Pr, Box<dyn Error>> {
    let mut pr = pr()?;
    let toolchain = include_str!("../../../../rust-toolchain.toml");
    let files = FIXTURE.into_iter().map(|(path, text)| {
        let new = replaced.iter().find(|(name, _)| *name == path);
        new.map_or((path, text), |(_, text)| (path, *text))
    });
    for (path, text) in files.chain([("rust-toolchain.toml", toolchain)]) {
        let file = pr.head_tree.join(path);
        fs::create_dir_all(file.parent().ok_or("a fixture file has a parent")?)?;
        fs::write(&file, text)?;
        pr.set(Revision::Head, path, text);
    }
    Ok(pr)
}

/// What a real backstop run gave: the outcome, each finding's rule, file and line, and the
/// backstop's stderr.
type RealRun = (GateJobOutcome, Vec<(String, String, u64)>, String);

/// Runs gates-lint over `pr` with the real backstop.
fn for_real(pr: &Pr) -> Result<RealRun, Box<dyn Error>> {
    let environment = GateEnvironment::from_vars(std::env::vars_os());
    let settings = TemperSettings::new(pr.paths(), &environment, "run-1")?;
    let mut fake = FakeLint::leaving(DIFF_TOTALS);
    let mut real = RealBackstop {
        fake: &mut fake,
        stderr: String::new(),
    };
    let (outcome, document) = outcome_with(pr, &mut real, settings)?;
    let found = shown(document)
        .into_iter()
        .map(|(rule, file, line, ..)| (rule, file, line))
        .collect();
    Ok((outcome, found, real.stderr))
}

/// Real cargo-clippy, both passes: an override only release compiles (`debug_assertions`
/// off) passes the debug pass and fails the release pass at its line (#172 F1).
#[test]
fn a_real_release_pass_refuses_an_override_under_not_debug_assertions() -> Result<(), Box<dyn Error>>
{
    let lib = "//! Lowers a forbidden lint in release builds only.\n\
               \n\
               macro_rules! hole {\n    ($lint:path) => {\n        #[allow($lint)]\n        \
               pub fn from_macro_rules() {}\n    };\n}\n\
               #[cfg(not(debug_assertions))]\n\
               hole!(unsafe_code);\n";
    let pr = fixture(Some(lib))?;

    let (outcome, found, stderr) = for_real(&pr)?;

    assert_eq!(outcome, FAILED, "{stderr}");
    assert_eq!(
        found,
        [("forbid-override".to_owned(), "fx/src/lib.rs".to_owned(), 10)],
        "{stderr}"
    );
    Ok(())
}

/// Real cargo-clippy (a two-crate check build, a few seconds): rustc reports E0453 for both
/// assembled overrides, the gate fails with both located, and nothing in the head's config
/// steered the run.
#[test]
fn a_real_build_refuses_overrides_a_macro_and_a_proc_macro_assemble() -> Result<(), Box<dyn Error>>
{
    let pr = fixture(None)?;

    let (outcome, found, stderr) = for_real(&pr)?;

    assert_eq!(outcome, FAILED, "{stderr}");
    assert_eq!(
        found,
        [
            ("forbid-override".to_owned(), "fx/src/lib.rs".to_owned(), 9),
            ("forbid-override".to_owned(), "fx/src/lib.rs".to_owned(), 10),
        ],
        "{stderr}"
    );
    // The scripted `baseline --check` leaves `target/temper/findings.json`; cargo would leave
    // `target/debug` and `target/release`.
    for profile in ["target/debug", "target/release"] {
        let built = pr.head_tree.join(profile);
        assert!(!built.exists(), "the backstop builds outside the head tree");
    }
    assert!(
        !pr.head_tree.join("elsewhere").exists(),
        "the config's target-dir is not used"
    );
    assert_eq!(
        backstop_dirs(&pr)?,
        Vec::<OsString>::new(),
        "its target directory is removed"
    );
    Ok(())
}

/// Real cargo-clippy: a head whose profiles turn `debug_assertions` off for dev and test hides
/// `cfg(debug_assertions)` code from both passes unless the backstop pins it (#172 round 3, ADV
/// F2). The profile change is also Z2s's `manifest-switch`; this pins the backstop's own
/// defence.
#[test]
fn a_real_debug_pass_pins_debug_assertions_whatever_the_profile_says() -> Result<(), Box<dyn Error>>
{
    let root = "[workspace]\nmembers = [\"fx\", \"mac\"]\nresolver = \"3\"\n\n\
                [profile.dev]\ndebug-assertions = false\n\n\
                [profile.test]\ndebug-assertions = false\n";
    let lib = "//! Lowers a forbidden lint where debug assertions are on.\n\
               \n\
               macro_rules! hole {\n    ($lint:path) => {\n        #[allow($lint)]\n        \
               pub fn from_macro_rules() {}\n    };\n}\n\
               #[cfg(debug_assertions)]\n\
               hole!(unsafe_code);\n";
    let pr = fixture_with(&[("Cargo.toml", root), ("fx/src/lib.rs", lib)])?;

    let (outcome, found, stderr) = for_real(&pr)?;

    assert_eq!(outcome, FAILED, "{stderr}");
    assert_eq!(
        found,
        [("forbid-override".to_owned(), "fx/src/lib.rs".to_owned(), 10)],
        "{stderr}"
    );
    Ok(())
}
