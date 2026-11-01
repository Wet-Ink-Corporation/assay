//! `plan` end to end (#55, `docs/architecture/04-pipeline.md` §2): fixture PRs built as git
//! repositories, fetched into forge's object-only repository and read through
//! `ForgePullRequest`, planned by `assay_app::plan` into a validated `assay.run/1`.

use std::env;
use std::error::Error;
use std::fs;
use std::num::NonZeroU64;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use assay_app::{
    ArtifactNonce, BaseConfigError, ContractValidator, EntryKind, FileListError, IncludeTargets,
    NonceSource, PackageSet, PlanError, PlanRequest, PlannedRun, PullRequestEvent, RestFile,
    RestFiles, plan,
};
use assay_cli::{ForgeObjectsError, ForgePullRequest};
use assay_domain::{ContentHash, RunUid};
use assay_forge::git::{CommitSha, GitEnvironment, GitLimits, ObjectRepo, TreeLimits};
use serde_json::{Value, json};
use tempfile::TempDir;

const PIN: &str = "0123456789abcdef0123456789abcdef01234567";

pub(crate) fn plugins_toml(required: bool) -> String {
    format!(
        "[assay]\ndigest = \"sha256:dc7c1d8e3ccab68cfdb828ce40a446c3913f7172f899ffe3f439839cfd917a8b\"\n\n\
         [[plugin]]\nname = \"temper\"\nadapter = \"builtin:temper\"\npin = \"git:{PIN}\"\n\
         required = {required}\n"
    )
}

/// A PR fetched into an object-only repository (shared with `plan_run`).
pub(crate) struct Pr {
    _scratch: TempDir,
    pub(crate) repo: ObjectRepo,
    pub(crate) base: CommitSha,
    pub(crate) head: CommitSha,
}

/// The fork point holds `.assay/plugins.toml`, a manifest and a library; `base_files` are
/// committed on top of it as the base, and `head_files` on top of the fork point as the head.
pub(crate) fn pr(
    base_files: &[(&str, &str)],
    head_files: &[(&str, &str)],
) -> Result<Pr, Box<dyn Error>> {
    pr_with_base_edit(
        |work| {
            for (path, contents) in base_files {
                write(work, path, contents)?;
            }
            Ok(())
        },
        head_files,
    )
}

/// Like [`pr`], with the base commit made by `edit_base` in the scratch worktree.
fn pr_with_base_edit(
    edit_base: impl FnOnce(&Path) -> Result<(), Box<dyn Error>>,
    head_files: &[(&str, &str)],
) -> Result<Pr, Box<dyn Error>> {
    let scratch = tempfile::tempdir()?;
    let work = scratch.path().join("work");
    plain_git(scratch.path(), &["init", "--quiet", "work"])?;
    let config = plugins_toml(true);
    write(&work, ".assay/plugins.toml", &config)?;
    write(
        &work,
        "Cargo.toml",
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
    )?;
    write(&work, "src/lib.rs", "pub fn one() {}\n")?;
    let fork = commit(&work, &[], "fork")?;
    edit_base(&work)?;
    let base = commit(&work, &[], "base")?;
    plain_git(&work, &["checkout", "--quiet", "-b", "pr", fork.as_str()])?;
    let head = commit(&work, head_files, "head")?;
    let git_dir = scratch.path().join("objects.git");
    let repo = ObjectRepo::init(
        &git_dir,
        GitEnvironment::from_process_env()?,
        GitLimits::default(),
    )?;
    let destination = git_dir.to_str().ok_or("scratch path is not UTF-8")?;
    let base_ref = format!("{base}:refs/heads/base");
    plain_git(
        &work,
        &[
            "push",
            "--quiet",
            destination,
            "pr:refs/heads/pr",
            &base_ref,
        ],
    )?;
    Ok(Pr {
        _scratch: scratch,
        repo,
        base,
        head,
    })
}

fn commit(work: &Path, files: &[(&str, &str)], message: &str) -> Result<CommitSha, Box<dyn Error>> {
    for (path, contents) in files {
        write(work, path, contents)?;
    }
    plain_git(work, &["add", "--all"])?;
    plain_git(work, &["commit", "--quiet", "--allow-empty", "-m", message])?;
    Ok(plain_git(work, &["rev-parse", "HEAD"])?.trim().parse()?)
}

fn write(root: &Path, path: &str, contents: &str) -> Result<(), Box<dyn Error>> {
    let path = root.join(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)?;
    Ok(())
}

/// Runs git as a developer would, isolated from the machine's config. Test setup only.
pub(crate) fn plain_git(cwd: &Path, args: &[&str]) -> Result<String, Box<dyn Error>> {
    let mut git = Command::new("git");
    git.env_clear();
    if let Some(path) = env::var_os("PATH") {
        git.env("PATH", path);
    }
    let output = git
        .current_dir(cwd)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

/// Nonces `0..`, as 32 hex digits.
#[derive(Debug, Default)]
pub(crate) struct Counting(u128);

#[derive(Debug)]
pub(crate) struct NoNonce;

impl std::fmt::Display for NoNonce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("no nonce")
    }
}

impl Error for NoNonce {}

impl NonceSource for Counting {
    type Error = NoNonce;

    fn nonce(&mut self) -> Result<ArtifactNonce, NoNonce> {
        let nonce = format!("{:032x}", self.0).parse().map_err(|_bad| NoNonce)?;
        self.0 = self.0.saturating_add(1);
        Ok(nonce)
    }
}

fn request(pr: &Pr, rest: &[&str]) -> Result<PlanRequest, Box<dyn Error>> {
    let files: Vec<RestFile> = rest
        .iter()
        .map(|path| RestFile {
            path: (*path).to_owned(),
            previous_path: None,
        })
        .collect();
    let one = NonZeroU64::new(1).ok_or("one is not zero")?;
    Ok(PlanRequest {
        pull_request: PullRequestEvent {
            repo: "Wet-Ink-Corporation/assay".to_owned(),
            number: NonZeroU64::new(55).ok_or("not zero")?,
            base_sha: pr.base.as_str().parse()?,
            head_sha: pr.head.as_str().parse()?,
            base_ref: "main".to_owned(),
            draft: false,
        },
        run_uid: RunUid {
            repo_id: one,
            run_id: one,
            run_attempt: one,
        },
        rest_files: RestFiles {
            changed_files: u64::try_from(files.len())?,
            files,
        },
        base_packages: Some(PackageSet::default()),
        head_packages: Some(PackageSet::default()),
        include_targets: IncludeTargets::Scanned(Vec::new()),
        prompt_bundle: ContentHash::of_files([]),
    })
}

fn run(
    pr: &Pr,
    request: &PlanRequest,
) -> Result<Result<PlannedRun, PlanError<ForgeObjectsError, NoNonce>>, Box<dyn Error>> {
    let objects = ForgePullRequest::new(&pr.repo, TreeLimits::default(), Duration::from_secs(60));
    Ok(plan(
        request,
        &objects,
        &mut Counting::default(),
        &ContractValidator::new()?,
    ))
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn plans_from_the_base_config_even_when_the_head_rewrites_it() -> Result<(), Box<dyn Error>> {
    // The head drops `required` from the only gate and edits the library.
    let pr = pr(
        &[],
        &[
            (".assay/plugins.toml", &plugins_toml(false)),
            ("src/lib.rs", "pub fn two() {}\n"),
        ],
    )?;
    let request = request(&pr, &[".assay/plugins.toml", "src/lib.rs"])?;

    let planned = run(&pr, &request)??;

    let doc: Value = serde_json::from_slice(planned.json())?;
    assert_eq!(
        (
            &doc["base_sha"],
            &doc["head_sha"],
            &doc["plugins"][0]["required"],
            &doc["trust_paths_changed"],
            &doc["tier"],
        ),
        (
            &json!(pr.base.as_str()),
            &json!(pr.head.as_str()),
            &json!(true),
            &json!([".assay/plugins.toml"]),
            &json!("full"),
        ),
        "base config governs the run; the head's edit is a trust-path change"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn lists_the_heads_files_from_the_merge_base_when_the_base_moved_on() -> Result<(), Box<dyn Error>>
{
    let pr = pr(
        &[("docs/landed.md", "on main after the fork\n")],
        &[("src/lib.rs", "pub fn two() {}\n")],
    )?;
    let request = request(&pr, &["src/lib.rs"])?;

    let planned = run(&pr, &request)??;

    assert_eq!(
        (
            planned.tiering().filtered.files,
            planned.tiering().filtered.lines
        ),
        (1, 2),
        "only the head's own change is planned, matching GitHub's list"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_rest_list_git_does_not_confirm_fails_the_plan() -> Result<(), Box<dyn Error>> {
    let pr = pr(&[], &[("src/lib.rs", "pub fn two() {}\n")])?;
    // The REST list hides the change and names another file.
    let request = request(&pr, &["README.md"])?;

    let planned = run(&pr, &request)?;

    assert!(
        matches!(
            planned,
            Err(PlanError::FileList(FileListError::Mismatch {
                only_in_git: 1,
                only_in_rest: 1,
                ..
            }))
        ),
        "a mismatch must fail loudly, got {planned:?}"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_submodule_in_the_base_config_fails_the_plan() -> Result<(), Box<dyn Error>> {
    let pr = pr_with_base_edit(
        |work| {
            // An unpopulated submodule's directory, so `git add --all` keeps the gitlink.
            fs::create_dir_all(work.join(".assay/vendored"))?;
            plain_git(
                work,
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    "160000,0123456789abcdef0123456789abcdef01234567,.assay/vendored",
                ],
            )?;
            Ok(())
        },
        &[("src/lib.rs", "pub fn two() {}\n")],
    )?;
    let request = request(&pr, &["src/lib.rs"])?;

    let planned = run(&pr, &request)?;

    assert!(
        matches!(
            planned,
            Err(PlanError::BaseConfig(BaseConfigError::NotARegularFile {
                ref path,
                kind: EntryKind::Submodule,
            })) if path == ".assay/vendored"
        ),
        "a gitlink under base `.assay/` is refused, never followed, got {planned:?}"
    );
    Ok(())
}
