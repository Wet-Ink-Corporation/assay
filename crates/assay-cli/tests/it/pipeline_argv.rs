//! The argv of every `bin/assay` step of `workflows/assay.yml.tmpl` (#60 ST-15, [`PIPELINE`]) and
//! of `workflows/assay-decide.yml.tmpl` (#61 DO-13, [`DECIDE_ONLY`]), as tables the template tests
//! (`pipeline_template.rs`, `decide_only_template.rs`) and the binary tests below both read: a
//! flag renamed in the binary fails here, and a template, which must use exactly these lines,
//! fails there.

use std::error::Error;
use std::process::Command;

use tempfile::TempDir;

use self::Arg::{Variable, Word};

/// Base config, from the sparse base checkout every job makes.
const CONFIG: &str = "base/.assay/plugins.toml";

/// One argument of an invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Arg {
    /// A literal word.
    Word(&'static str),
    /// A variable the step's `env:` sets, written `"$NAME"` in the `run:` line.
    Variable(&'static str),
}

/// One `bin/assay` step of a template.
#[derive(Debug)]
pub(crate) struct Invocation {
    /// The job it runs in.
    pub(crate) job: &'static str,
    /// The arguments after `bin/assay`.
    pub(crate) args: &'static [Arg],
    /// The variables the step's `env:` sets: the GitHub-set ones (`GITHUB_*`, `CI`, `HOME`,
    /// `PATH`) are the runner's and not listed.
    pub(crate) env: &'static [&'static str],
    /// What the command writes to stderr for an argument it does not take.
    pub(crate) refusal: &'static str,
}

impl Invocation {
    /// The step's `run:` value.
    pub(crate) fn run_line(&self) -> String {
        let words = self.args.iter().map(|arg| match arg {
            Word(word) => (*word).to_owned(),
            Variable(name) => format!("\"${name}\""),
        });
        std::iter::once("bin/assay".to_owned())
            .chain(words)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

// The variables of 03 §4a: the upstream outputs, the `assay-verdict` App's token, the
// `assay-ledger-writer` token, the job's `github.token`, and the App's ids.
const NEEDS: &str = "ASSAY_NEEDS";
const TOKEN: &str = "ASSAY_GITHUB_TOKEN";
const LEDGER_TOKEN: &str = "ASSAY_LEDGER_TOKEN";
const RUN_TOKEN: &str = "ASSAY_RUN_TOKEN";
const APP_ID: &str = "ASSAY_VERDICT_APP_ID";
const BOT_ID: &str = "ASSAY_VERDICT_BOT_ID";

/// The pinned commits and run key every gates job reads.
const GATE_ENV: [&str; 3] = ["ASSAY_RUN_KEY", "ASSAY_BASE_SHA", "ASSAY_HEAD_SHA"];

/// What `decide` and `publish` both read (03 §4a).
const DECIDE_ENV: [&str; 6] = [NEEDS, TOKEN, LEDGER_TOKEN, RUN_TOKEN, APP_ID, BOT_ID];

/// The pipeline's `bin/assay` steps, in the template's job order (03 §4a, the #60 argv table).
pub(crate) const PIPELINE: &[Invocation] = &[
    Invocation {
        job: "guard",
        args: &[Word("guard")],
        env: &[RUN_TOKEN],
        refusal: "guard takes no arguments",
    },
    Invocation {
        job: "open",
        args: &[Word("open"), Word("--config"), Word(CONFIG)],
        env: &[NEEDS, TOKEN, LEDGER_TOKEN, APP_ID, BOT_ID],
        refusal: "usage: assay open",
    },
    Invocation {
        job: "plan",
        args: &[Word("plan"), Word("--out"), Word("plan-out")],
        env: &[NEEDS, RUN_TOKEN],
        refusal: "usage: assay plan",
    },
    Invocation {
        job: "gates-static",
        args: &[
            Word("gate"),
            Word("gates-static"),
            Word("--config"),
            Word(CONFIG),
            Word("--head"),
            Word("pr"),
            Word("--out"),
            Word("gates-out"),
            Word("--node"),
            Variable("ASSAY_NODE"),
            Word("--temper"),
            Variable("ASSAY_TEMPER"),
        ],
        env: &[
            GATE_ENV[0],
            GATE_ENV[1],
            GATE_ENV[2],
            RUN_TOKEN,
            "ASSAY_NODE",
            "ASSAY_TEMPER",
        ],
        refusal: "usage: assay gate",
    },
    Invocation {
        job: "gates-lint",
        args: &[
            Word("gate"),
            Word("gates-lint"),
            Word("--config"),
            Word(CONFIG),
            Word("--head"),
            Word("pr"),
            Word("--out"),
            Word("gates-out"),
            Word("--node"),
            Variable("ASSAY_NODE"),
            Word("--temper"),
            Variable("ASSAY_TEMPER"),
        ],
        env: &[
            GATE_ENV[0],
            GATE_ENV[1],
            GATE_ENV[2],
            "ASSAY_MERGE_BASE",
            RUN_TOKEN,
            "ASSAY_NODE",
            "ASSAY_TEMPER",
        ],
        refusal: "usage: assay gate",
    },
    Invocation {
        job: "gates-test",
        args: &[
            Word("gate"),
            Word("gates-test"),
            Word("--config"),
            Word(CONFIG),
            Word("--head"),
            Word("pr"),
            Word("--out"),
            Word("gates-out"),
        ],
        env: &GATE_ENV,
        refusal: "usage: assay gate",
    },
    Invocation {
        job: "admit",
        args: &[
            Word("admit"),
            Word("--config"),
            Word(CONFIG),
            Word("--artifacts"),
            Word("artifacts"),
            Word("--out"),
            Word("admit-out"),
        ],
        env: &[NEEDS, TOKEN, LEDGER_TOKEN, RUN_TOKEN],
        refusal: "usage: assay admit",
    },
    Invocation {
        job: "review",
        args: &[Word("review"), Word("--placeholder")],
        env: &[],
        refusal: "usage: assay review",
    },
    Invocation {
        job: "release",
        args: &[Word("release"), Word("--config"), Word(CONFIG)],
        env: &[NEEDS, LEDGER_TOKEN],
        refusal: "usage: assay release",
    },
    Invocation {
        job: "decide",
        args: &[
            Word("decide"),
            Word("--config"),
            Word(CONFIG),
            Word("--artifacts"),
            Word("artifacts"),
            Word("--out"),
            Word("decide-out"),
        ],
        env: &DECIDE_ENV,
        refusal: "usage: assay decide",
    },
    Invocation {
        job: "decide",
        args: &[
            Word("publish"),
            Word("--config"),
            Word(CONFIG),
            Word("--state"),
            Word("decide-out"),
        ],
        env: &[
            DECIDE_ENV[0],
            DECIDE_ENV[1],
            DECIDE_ENV[2],
            DECIDE_ENV[3],
            DECIDE_ENV[4],
            DECIDE_ENV[5],
            "ASSAY_PERSIST",
        ],
        refusal: "usage: assay publish",
    },
];

/// The matrix entry the decide-only `redecide` job decides (03 §4a).
const PR: &str = "ASSAY_PR";

/// What decide-only's `decide` and `publish` both read (03 §4a): decide's, and the PR.
const REDECIDE_ENV: [&str; 7] = [NEEDS, PR, TOKEN, LEDGER_TOKEN, RUN_TOKEN, APP_ID, BOT_ID];

/// The decide-only workflow's `bin/assay` steps, in the template's job order (#61 DO-13, D9 A,
/// 03 §4a).
pub(crate) const DECIDE_ONLY: &[Invocation] = &[
    Invocation {
        job: "guard",
        args: &[Word("decide-only"), Word("guard")],
        env: &[RUN_TOKEN, APP_ID, BOT_ID],
        refusal: "usage: assay decide-only guard",
    },
    Invocation {
        job: "redecide",
        args: &[
            Word("decide-only"),
            Word("decide"),
            Word("--config"),
            Word(CONFIG),
            Word("--out"),
            Word("decide-out"),
        ],
        env: &REDECIDE_ENV,
        refusal: "usage: assay decide-only decide",
    },
    Invocation {
        job: "redecide",
        args: &[
            Word("decide-only"),
            Word("publish"),
            Word("--config"),
            Word(CONFIG),
            Word("--state"),
            Word("decide-out"),
        ],
        // Entry by entry, since a const slice cannot be concatenated with another.
        env: &[
            REDECIDE_ENV[0],
            REDECIDE_ENV[1],
            REDECIDE_ENV[2],
            REDECIDE_ENV[3],
            REDECIDE_ENV[4],
            REDECIDE_ENV[5],
            REDECIDE_ENV[6],
            "ASSAY_PERSIST",
        ],
        refusal: "usage: assay decide-only publish",
    },
];

/// Runs the binary with `args` in an empty directory, with no variable but a `PATH` that finds
/// nothing, so every command stops at its first missing variable and reaches no program; a
/// [`Variable`] is given as an absolute path that does not exist. Returns its exit code and stderr.
fn run(args: &[Arg], extra: &[&str]) -> Result<(Option<i32>, String), Box<dyn Error>> {
    let scratch = TempDir::new()?;
    let path = scratch.path().join("bin");
    let words = args.iter().map(|arg| match arg {
        Word(word) => (*word).to_owned(),
        Variable(name) => format!("/nonexistent/{name}"),
    });
    let ran = Command::new(env!("CARGO_BIN_EXE_assay"))
        .args(words)
        .args(extra)
        .current_dir(scratch.path())
        .env_clear()
        .env("PATH", path)
        .output()?;
    Ok((ran.status.code(), String::from_utf8(ran.stderr)?))
}

/// Asserts that each argv of `table` gets past argument parsing: it exits 2 at its first
/// missing variable, not with its usage refusal.
fn assert_each_parses(table: &[Invocation]) -> Result<(), Box<dyn Error>> {
    for invocation in table {
        let (code, stderr) = run(invocation.args, &[])?;

        assert_eq!(code, Some(2), "{}: {stderr}", invocation.run_line());
        assert!(
            !stderr.contains(invocation.refusal),
            "{} is refused as a usage error: {stderr}",
            invocation.run_line()
        );
    }
    Ok(())
}

/// Asserts that each argv of `table` with one more flag is refused with its usage line.
fn assert_each_refuses_an_extra_flag(table: &[Invocation]) -> Result<(), Box<dyn Error>> {
    for invocation in table {
        let (code, stderr) = run(invocation.args, &["--bogus"])?;

        assert_eq!(code, Some(2), "{} --bogus: {stderr}", invocation.run_line());
        assert!(
            stderr.contains(invocation.refusal),
            "{} --bogus is not refused as a usage error: {stderr}",
            invocation.run_line()
        );
    }
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn every_pipeline_argv_gets_past_argument_parsing() -> Result<(), Box<dyn Error>> {
    assert_each_parses(PIPELINE)
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn every_pipeline_argv_with_an_extra_flag_is_a_usage_error() -> Result<(), Box<dyn Error>> {
    assert_each_refuses_an_extra_flag(PIPELINE)
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn every_decide_only_argv_gets_past_argument_parsing() -> Result<(), Box<dyn Error>> {
    assert_each_parses(DECIDE_ONLY)
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn every_decide_only_argv_with_an_extra_flag_is_a_usage_error() -> Result<(), Box<dyn Error>> {
    assert_each_refuses_an_extra_flag(DECIDE_ONLY)
}

#[test]
fn a_variable_is_written_quoted_in_the_run_line() {
    let line = Invocation {
        job: "gates-static",
        args: &[Word("gate"), Word("--node"), Variable("ASSAY_NODE")],
        env: &[],
        refusal: "",
    }
    .run_line();

    assert_eq!(line, "bin/assay gate --node \"$ASSAY_NODE\"");
}
