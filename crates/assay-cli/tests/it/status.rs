//! `assay status` (#126): its arguments, its wiring of `assay_app::status` over fakes of the
//! read port and the artifact store, and the binary's refusals before it reads GitHub
//! (`status_pr.rs` drives it end to end through a stand-in `gh`).

use std::collections::BTreeSet;
use std::error::Error;
use std::ffi::OsString;
use std::io::{self, Write};
use std::num::NonZeroU64;
use std::process::Command;

use assay_app::{
    AppId, BundleSource, CheckView, CommitId, Contract, ContractValidator, Detail, FetchedAt,
    Identities, PullView, ReviewCommentView, StatusForge, StatusPorts, StatusRepository,
    StoredBundle, UserId, Viewer,
};
use assay_cli::{
    Format, StatusArgs, StatusCommand, StatusContext, StatusRunError, StatusUsage, run_status,
};
use assay_domain::{Rubric, RunUid};

const HEAD: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn args(words: &[&str]) -> Vec<OsString> {
    words.iter().map(OsString::from).collect()
}

#[test]
fn no_arguments_mean_text_for_the_current_branch_without_bodies() {
    assert_eq!(
        StatusArgs::parse(args(&[])),
        Ok(StatusArgs {
            format: Format::Text,
            pr: None,
            detail: Detail::Summary,
        })
    );
}

#[test]
fn every_flag_is_read() {
    assert_eq!(
        StatusArgs::parse(args(&["--json", "--pr", "42", "--full"])),
        Ok(StatusArgs {
            format: Format::Json,
            pr: NonZeroU64::new(42),
            detail: Detail::Full,
        })
    );
}

#[test]
fn bad_arguments_are_usage_errors() {
    let cases = [
        (&["--bogus"][..], StatusUsage::Unknown),
        (&["42"][..], StatusUsage::Unknown),
        (&["--json", "--json"][..], StatusUsage::Repeated("--json")),
        (&["--full", "--full"][..], StatusUsage::Repeated("--full")),
        (
            &["--pr", "1", "--pr", "2"][..],
            StatusUsage::Repeated("--pr"),
        ),
        (&["--pr"][..], StatusUsage::PrNumber),
        (&["--pr", "0"][..], StatusUsage::PrNumber),
        (&["--pr", "+4"][..], StatusUsage::PrNumber),
        (&["--pr", "x"][..], StatusUsage::PrNumber),
    ];

    for (words, usage) in cases {
        assert_eq!(StatusArgs::parse(args(words)), Err(usage), "{words:?}");
    }
}

/// PR 42 from `feature/status`, with no check and no comment, viewed by user 3.
struct FakeGitHub {
    pull: PullView,
}

#[derive(Debug)]
struct FakeError;

impl std::fmt::Display for FakeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("fake failure")
    }
}

impl Error for FakeError {}

impl FakeGitHub {
    fn new() -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            pull: PullView {
                number: NonZeroU64::new(42).ok_or("not zero")?,
                head_sha: HEAD.parse()?,
                head_ref: "feature/status".to_owned(),
                base_ref: "main".to_owned(),
            },
        })
    }
}

impl StatusForge for FakeGitHub {
    type Error = FakeError;

    fn pull(&self, number: NonZeroU64) -> Result<Option<PullView>, FakeError> {
        Ok((number == self.pull.number).then(|| self.pull.clone()))
    }

    fn open_pulls(&self, branch: &str, _limit: usize) -> Result<Vec<PullView>, FakeError> {
        Ok(if branch == self.pull.head_ref {
            vec![self.pull.clone()]
        } else {
            Vec::new()
        })
    }

    fn check_runs(&self, _head: &CommitId, _limit: usize) -> Result<Vec<CheckView>, FakeError> {
        Ok(Vec::new())
    }

    fn review_comments(
        &self,
        _number: NonZeroU64,
        _app: AppId,
        _limit: usize,
    ) -> Result<Vec<ReviewCommentView>, FakeError> {
        Ok(Vec::new())
    }

    fn viewer(&self) -> Result<Viewer, FakeError> {
        Ok(Viewer {
            id: UserId(NonZeroU64::MIN.saturating_add(2)),
            login: "someone".to_owned(),
        })
    }
}

impl BundleSource for FakeGitHub {
    type Error = FakeError;

    fn fetch(&self, _run: RunUid, _file: &str, _limit: usize) -> Result<StoredBundle, FakeError> {
        Ok(StoredBundle::Missing)
    }
}

/// A writer that fails every write.
struct Closed;

impl Write for Closed {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::BrokenPipe.into())
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::ErrorKind::BrokenPipe.into())
    }
}

/// What `run_status` gives over the fake.
type Outcome = Result<(), StatusRunError<assay_app::StatusError<FakeError, FakeError>>>;

/// Runs `assay status` with `words` over the fake, on branch `branch`, into `out`.
fn run(words: &[&str], branch: &str, out: impl Write) -> Result<Outcome, Box<dyn Error>> {
    let parsed = StatusArgs::parse(args(words))?;
    let repository = StatusRepository {
        id: NonZeroU64::new(840_213_377).ok_or("not zero")?,
        name: "Wet-Ink-Corporation/assay".to_owned(),
        default_branch: "main".to_owned(),
    };
    let rubric: Rubric = serde_json::from_value(serde_json::json!({
        "version": "1",
        "majors_block_threshold": 3,
        "max_requeues": 3,
        "micro_roles": ["combined-micro"],
    }))?;
    let identities = Identities {
        privileged: BTreeSet::new(),
        agents: BTreeSet::new(),
    };
    let local: CommitId = HEAD.parse()?;
    let validator = ContractValidator::new()?;
    let fetched_at: FetchedAt = "2026-10-13T09:30:00Z".parse()?;
    let context = StatusContext {
        repository: &repository,
        app: AppId(NonZeroU64::new(424_242).ok_or("not zero")?),
        branch,
        local_head: Some(&local),
        rubric: &rubric,
        identities: &identities,
        fetched_at: &fetched_at,
        validator: &validator,
    };
    let github = FakeGitHub::new()?;
    let ports = StatusPorts {
        forge: &github,
        bundles: &github,
    };
    Ok(run_status(&parsed, &context, &ports, out))
}

#[test]
fn json_is_a_valid_status_document_of_the_named_pr() -> Result<(), Box<dyn Error>> {
    let mut out = Vec::new();

    run(&["--json", "--pr", "42"], "elsewhere", &mut out)??;

    let document = ContractValidator::new()?.read(Contract::Status, &out)?;
    assert_eq!(
        (document["pr"].as_u64(), document["check"].is_null()),
        (Some(42), true)
    );
    Ok(())
}

#[test]
fn without_pr_the_current_branchs_open_pr_is_shown_as_text() -> Result<(), Box<dyn Error>> {
    let mut out = Vec::new();

    run(&[], "feature/status", &mut out)??;

    let text = String::from_utf8(out)?;
    assert_eq!(
        text.lines().take(3).collect::<Vec<_>>(),
        [
            "assay status: Wet-Ink-Corporation/assay #42 (feature/status)",
            "Head: bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb (local HEAD matches)",
            "Check: none yet",
        ]
    );
    Ok(())
}

#[test]
fn a_refused_status_writes_nothing() -> Result<(), Box<dyn Error>> {
    let mut out = Vec::new();

    let outcome = run(&["--json"], "no-open-pr", &mut out)?;

    assert!(
        matches!(
            outcome,
            Err(StatusRunError::Status(assay_app::StatusError::NoOpenPull))
        ),
        "{outcome:?}"
    );
    assert_eq!(out, b"");
    Ok(())
}

#[test]
fn a_closed_output_is_reported() -> Result<(), Box<dyn Error>> {
    let outcome = run(&["--json", "--pr", "42"], "elsewhere", Closed)?;

    assert!(
        matches!(&outcome, Err(StatusRunError::Write(error)) if error.kind() == io::ErrorKind::BrokenPipe),
        "{outcome:?}"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn the_binary_refuses_bad_arguments_and_a_missing_token() -> Result<(), Box<dyn Error>> {
    let bad = Command::new(env!("CARGO_BIN_EXE_assay"))
        .args(["status", "--bogus"])
        .output()?;
    let untokened = Command::new(env!("CARGO_BIN_EXE_assay"))
        .args(["status", "--json"])
        .env_remove("ASSAY_GITHUB_TOKEN")
        .output()?;

    assert_eq!((bad.status.code(), bad.stdout.is_empty()), (Some(2), true));
    assert_eq!(
        (untokened.status.code(), untokened.stdout.is_empty()),
        (Some(2), true)
    );
    let message = String::from_utf8(untokened.stderr)?;
    assert!(message.contains("ASSAY_GITHUB_TOKEN"), "{message}");
    Ok(())
}

#[test]
fn local_is_asked_for_with_text_or_json() {
    let asked = [
        &["--local"][..],
        &["--json", "--local"],
        &["--local", "--json"],
    ]
    .map(|words| StatusCommand::parse(args(words)));

    assert_eq!(
        asked,
        [
            Ok(StatusCommand::Local(Format::Text)),
            Ok(StatusCommand::Local(Format::Json)),
            Ok(StatusCommand::Local(Format::Json)),
        ]
    );
}

#[test]
fn without_local_a_prs_arguments_are_read() {
    let asked = StatusCommand::parse(args(&["--json", "--pr", "42"]));

    assert_eq!(
        asked,
        Ok(StatusCommand::Pull(StatusArgs {
            format: Format::Json,
            pr: NonZeroU64::new(42),
            detail: Detail::Summary,
        }))
    );
}

#[test]
fn local_with_a_prs_argument_or_given_twice_is_a_usage_error() {
    for (words, usage) in [
        (&["--local", "--pr", "4"][..], StatusUsage::NotLocal("--pr")),
        (&["--full", "--local"], StatusUsage::NotLocal("--full")),
        (&["--local", "--local"], StatusUsage::Repeated("--local")),
        (&["--local", "--bogus"], StatusUsage::Unknown),
    ] {
        assert_eq!(StatusCommand::parse(args(words)), Err(usage), "{words:?}");
    }
}
