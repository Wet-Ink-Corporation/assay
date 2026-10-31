//! `run_plan` (#60 ST-5, S1) over fixture PRs fetched into forge's object-only repository, as
//! `plan.rs` builds them: the `run.json` it writes into `--out`, and plan's outputs (03 §4a)
//! read off the document it wrote.

use std::assert_matches;
use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::time::Duration;

use assay_app::{
    ArtifactJob, CommitId, ContractValidator, FileListError, PlanError, RestFile, RestFiles,
    Sha256Digest, UserId,
};
use assay_cli::outputs::{Identity, PlanOutputs, Trigger};
use assay_cli::{
    ForgePullRequest, JobName, Needs, OutDir, OutDirError, OutputName, Outputs, PlanJob, PlanPorts,
    PlanRunError, RUN_FILE, RunEnv, RunVariables, run_plan,
};
use assay_domain::{ContentHash, Tier};
use assay_forge::git::TreeLimits;
use serde_json::Value;
use tempfile::TempDir;

use crate::plan::{Counting, NoNonce, Pr, plain_git, pr};

fn run_env() -> Result<RunEnv, Box<dyn Error>> {
    Ok(RunEnv::from_vars(&RunVariables {
        run_id: Some(OsString::from("4242")),
        run_attempt: Some(OsString::from("1")),
        repository_id: Some(OsString::from("42")),
        repository: Some(OsString::from("Wet-Ink-Corporation/assay")),
    })?)
}

fn id(n: u64) -> Result<NonZeroU64, Box<dyn Error>> {
    Ok(NonZeroU64::new(n).ok_or("a positive id")?)
}

/// guard's identity of `pr`, PR 55 into `main`.
fn identity(pr: &Pr, draft: bool) -> Result<Identity, Box<dyn Error>> {
    Ok(Identity {
        pr: id(55)?,
        head_sha: pr.head.as_str().parse()?,
        base_sha: pr.base.as_str().parse()?,
        base_ref: "main".to_owned(),
        draft,
        author: UserId(id(7)?),
        sender: UserId(id(7)?),
        trigger: Trigger::Opened,
        default_branch: "main".to_owned(),
    })
}

fn rest(paths: &[&str]) -> Result<RestFiles, Box<dyn Error>> {
    Ok(RestFiles {
        changed_files: u64::try_from(paths.len())?,
        files: paths
            .iter()
            .map(|path| RestFile {
                path: (*path).to_owned(),
                previous_path: None,
            })
            .collect(),
    })
}

/// A scratch directory whose `plan-out` is a valid `--out`.
struct Out {
    _dir: TempDir,
    path: PathBuf,
}

impl Out {
    fn new() -> Result<Self, Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        let path = fs::canonicalize(dir.path())?.join("plan-out");
        Ok(Self { _dir: dir, path })
    }
}

type Ran = Result<PlanOutputs, PlanRunError<NoNonce>>;

/// `run_plan` over `pr` with `rest` as GitHub's list, guard's `draft`, into `out`.
fn planned(pr: &Pr, rest: RestFiles, draft: bool, out: &Out) -> Result<Ran, Box<dyn Error>> {
    let run = run_env()?;
    let objects = ForgePullRequest::new(&pr.repo, TreeLimits::default(), Duration::from_secs(60));
    let validator = ContractValidator::new()?;
    let out = OutDir::new(&out.path)?;
    Ok(run_plan(
        PlanJob {
            run: &run,
            identity: identity(pr, draft)?,
            rest_files: rest,
            prompt_bundle: ContentHash::of_files([]),
        },
        PlanPorts {
            objects: &objects,
            nonces: &mut Counting::default(),
            validator: &validator,
        },
        &out,
    ))
}

/// A ready PR changing the library.
fn library_change() -> Result<(Pr, Out, PlanOutputs), Box<dyn Error>> {
    let pr = pr(&[], &[("src/lib.rs", "pub fn two() {}\n")])?;
    let out = Out::new()?;
    let outputs = planned(&pr, rest(&["src/lib.rs"])?, false, &out)??;
    Ok((pr, out, outputs))
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn the_document_is_written_to_out_and_its_digest_is_the_run_digest() -> Result<(), Box<dyn Error>> {
    let (_pr, out, outputs) = library_change()?;

    let written = fs::read(out.path.join(RUN_FILE))?;

    assert_eq!(outputs.run_digest(), &Sha256Digest::of(&written));
    assert_eq!(
        fs::read_dir(&out.path)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<Result<Vec<_>, _>>()?,
        [RUN_FILE],
        "only the document is in the artifact's directory"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn the_run_key_and_artifact_outputs_are_the_documents() -> Result<(), Box<dyn Error>> {
    let (_pr, out, outputs) = library_change()?;
    let doc: Value = serde_json::from_slice(&fs::read(out.path.join(RUN_FILE))?)?;

    let written = outputs.outputs();

    let value = |name: OutputName| {
        written
            .iter()
            .find(|(own, _)| *own == name)
            .map(|(_, value)| Value::String(value.clone()))
    };
    assert_eq!(value(OutputName::RunKey).as_ref(), Some(&doc["run_key"]));
    for job in ArtifactJob::ALL {
        assert_eq!(
            value(OutputName::artifact(job)).as_ref(),
            Some(&doc["artifact_names"][job.name()]),
            "{job:?}"
        );
    }
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_consumer_reads_back_exactly_what_plan_wrote() -> Result<(), Box<dyn Error>> {
    let (_pr, _out, outputs) = library_change()?;
    let mut file = Vec::new();
    let mut writer = Outputs::new(&mut file);
    for (name, value) in outputs.outputs() {
        writer.write(name, &value)?;
    }
    let text = String::from_utf8(file)?;
    let written: serde_json::Map<String, Value> = text
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| (name.to_owned(), Value::from(value)))
        .collect();
    let needs = serde_json::json!({"plan": {"result": "success", "outputs": written}});

    let read = PlanOutputs::from_needs(&Needs::parse(
        needs.to_string().as_bytes(),
        &[JobName::Plan],
    )?)?;

    assert_eq!(read, outputs);
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn the_merge_base_is_gits_merge_base_of_the_pinned_commits() -> Result<(), Box<dyn Error>> {
    let pr = pr(
        &[("docs/landed.md", "on main after the fork\n")],
        &[("src/lib.rs", "pub fn two() {}\n")],
    )?;
    let out = Out::new()?;
    let expected: CommitId = plain_git(
        pr.repo.git_dir(),
        &["merge-base", pr.base.as_str(), pr.head.as_str()],
    )?
    .trim()
    .parse()?;

    let outputs = planned(&pr, rest(&["src/lib.rs"])?, false, &out)??;

    assert_eq!(outputs.merge_base(), &expected);
    assert_ne!(
        expected.as_str(),
        pr.base.as_str(),
        "the base moved on, so the merge base is the fork point"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_ready_pr_with_an_llm_tier_is_reviewed_and_a_draft_is_not() -> Result<(), Box<dyn Error>> {
    let pr = pr(&[], &[("src/lib.rs", "pub fn two() {}\n")])?;
    let mut read = Vec::new();

    for draft in [false, true] {
        let out = Out::new()?;
        let outputs = planned(&pr, rest(&["src/lib.rs"])?, draft, &out)??;
        read.push((outputs.tier(), outputs.draft(), outputs.review()));
    }

    assert_eq!(read, [(Tier::Full, false, true), (Tier::Full, true, false)]);
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_change_that_needs_a_split_is_not_reviewed() -> Result<(), Box<dyn Error>> {
    let big = "line\n".repeat(5_001);
    let pr = pr(&[], &[("docs/big.md", &big)])?;
    let out = Out::new()?;

    let outputs = planned(&pr, rest(&["docs/big.md"])?, false, &out)??;

    assert_eq!(
        (outputs.tier(), outputs.review()),
        (Tier::NeedsSplit, false)
    );
    Ok(())
}

/// D2 B (Weigh-In, two-way): `plan` computes no package sets, so the package set reads as
/// unavailable, every Rust change is exec surface and the run is `full`. Computing them
/// (D2 A) changes this test deliberately.
#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn without_package_sets_a_rust_change_is_exec_surface() -> Result<(), Box<dyn Error>> {
    let (_pr, out, outputs) = library_change()?;
    let doc: Value = serde_json::from_slice(&fs::read(out.path.join(RUN_FILE))?)?;

    assert_eq!(
        (
            &doc["classes"],
            &doc["exec_surface_changed"],
            outputs.tier()
        ),
        (
            &serde_json::json!(["exec-surface"]),
            &serde_json::json!(["package set (unavailable)", "src/lib.rs"]),
            Tier::Full
        )
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_rest_list_git_does_not_confirm_fails_and_writes_nothing() -> Result<(), Box<dyn Error>> {
    let pr = pr(&[], &[("src/lib.rs", "pub fn two() {}\n")])?;
    let out = Out::new()?;

    let ran = planned(&pr, rest(&["README.md"])?, false, &out)?;

    assert_matches!(
        ran,
        Err(PlanRunError::Plan(PlanError::FileList(
            FileListError::Mismatch { .. }
        )))
    );
    assert!(!out.path.exists(), "no output directory is created");
    Ok(())
}

#[test]
fn an_out_that_already_exists_is_refused() -> Result<(), Box<dyn Error>> {
    let out = Out::new()?;
    fs::create_dir(&out.path)?;

    let refused = OutDir::new(&out.path);

    assert_matches!(refused, Err(OutDirError::Exists(path)) if path == out.path);
    Ok(())
}
