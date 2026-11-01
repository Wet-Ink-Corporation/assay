//! The run bundle read from the decide job's Actions artifact (#273, wi-6b26d6): the run's
//! artifacts listed with `gh api`, the one decide artifact downloaded with `gh run download`
//! into a scratch directory (gh extracts the zip; assay never does), and its one file read under
//! the limit. Over a fake runner that answers both; the process itself runs in `assay-cli`.

use std::cell::RefCell;
use std::error::Error;
use std::fs;
use std::io;
use std::num::NonZeroU64;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;

use assay_app::{
    AppId, BUNDLE_FILE, BundleSource, DECISION_FILE, DecisionBundleSource,
    MAX_DECISION_BUNDLE_BYTES, StoredBundle, UserId,
};
use assay_domain::RunUid;
use assay_forge::github::{
    Api, AppIdentity, BundleFetchError, GH_DOWNLOAD_TIMEOUT, GhApi, GhCall, GhDownloadError,
    GhEnvironment, GhRunner, GhToken, GitHub, ThreadSleep,
};
use assay_forge::process::{Captured, Limits, ProcessError};
use serde_json::{Value, json};

/// The repository's id, which keys its runs.
const REPO_ID: u64 = 1_072_347_811;
/// The run the pointer names.
const RUN_ID: u64 = 7;
/// The decide job's artifact, as `plan` names it.
const DECIDE: &str = "assay-decide-0123456789abcdef0123456789abcdef";
/// The token gh is given, which must never appear in an error.
const TOKEN: &str = "ghs_secret_token";

/// What the fake gh writes into the directory `gh run download` is given.
type Extract = Box<dyn Fn(&Path) -> io::Result<()>>;

/// gh, scripted: `gh api` answers the artifact listing; `gh run download` runs `extract` on its
/// `--dir` and exits with `exit`.
struct ScriptedGh {
    /// What `gh api --include` writes for the listing.
    listing: Vec<u8>,
    extract: Extract,
    exit: i32,
    stderr: Vec<u8>,
    /// Whether the supervisor stops the download past its directory cap after `extract`.
    killed: bool,
    calls: RefCell<Vec<Vec<String>>>,
    limits: RefCell<Vec<Limits>>,
    /// Each download's directory cap: its bytes and entries.
    caps: RefCell<Vec<(u64, usize)>>,
    dirs: RefCell<Vec<PathBuf>>,
}

impl ScriptedGh {
    /// gh listing `artifacts` and extracting `extract` with success.
    fn new(artifacts: &[Value], extract: Extract) -> Self {
        let body = json!({"total_count": artifacts.len(), "artifacts": artifacts});
        Self {
            listing: format!("HTTP/2.0 200 OK\n\r\n{body}").into_bytes(),
            extract,
            exit: 0,
            stderr: Vec::new(),
            killed: false,
            calls: RefCell::new(Vec::new()),
            limits: RefCell::new(Vec::new()),
            caps: RefCell::new(Vec::new()),
            dirs: RefCell::new(Vec::new()),
        }
    }

    /// The same gh, stopped by the supervisor past its directory cap after extracting.
    fn killed(self) -> Self {
        Self {
            killed: true,
            ..self
        }
    }

    /// The same gh, exiting `exit` after writing `stderr` on a download.
    fn failing(self, exit: i32, stderr: &[u8]) -> Self {
        Self {
            exit,
            stderr: stderr.to_vec(),
            ..self
        }
    }

    /// The arguments of every `gh run download`.
    fn downloads(&self) -> Vec<Vec<String>> {
        self.calls
            .borrow()
            .iter()
            .filter(|args| args.first().is_some_and(|first| first == "run"))
            .cloned()
            .collect()
    }
}

impl GhRunner for ScriptedGh {
    fn run(&self, call: &GhCall<'_>) -> Result<Captured, ProcessError> {
        self.limits.borrow_mut().push(call.limits);
        let args: Vec<String> = call.args.iter().map(|&arg| arg.to_owned()).collect();
        let api = args.first().is_some_and(|first| first == "api");
        if !api {
            self.extract_into(call, &args);
        }
        self.calls.borrow_mut().push(args);
        if !api && self.killed {
            return Err(ProcessError::DirectoryTooLarge);
        }
        // The test owns these bytes and hands gh's "output" over as a copy each run.
        let (stdout, exit, stderr) = if api {
            (self.listing.clone(), 0, Vec::new())
        } else {
            (Vec::new(), self.exit, self.stderr.clone())
        };
        Ok(Captured {
            status: ExitStatus::from_raw(exit << 8),
            stdout,
            stderr,
        })
    }
}

impl ScriptedGh {
    /// Runs the extraction on the `--dir` of `args`, which must exist, be empty, be mode 0700
    /// and be the directory `call` caps; the call's output cap must be 64 KiB.
    fn extract_into(&self, call: &GhCall<'_>, args: &[String]) {
        let dir = args
            .iter()
            .position(|arg| arg == "--dir")
            .and_then(|at| args.get(at + 1))
            .map(PathBuf::from)
            .expect("gh run download is given --dir");
        assert!(dir.is_dir(), "the directory exists before gh runs");
        assert_eq!(
            fs::read_dir(&dir).expect("readable").count(),
            0,
            "and is empty"
        );
        let mode = fs::metadata(&dir).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "only this user may read the download");
        assert_eq!(call.limits.max_output_bytes, 64 << 10);
        let cap = call.directory.expect("a download is capped");
        assert_eq!(cap.dir, dir, "the cap watches the download's directory");
        self.caps
            .borrow_mut()
            .push((cap.max_bytes, cap.max_entries));
        (self.extract)(&dir).expect("the fake extracts");
        self.dirs.borrow_mut().push(dir);
    }
}

/// A live artifact of the pointer's run, `size` bytes zipped, as GitHub lists it.
fn artifact(name: &str, size: u64) -> Value {
    json!({
        "id": 4_200_000_001_u64,
        "node_id": "MDg6QXJ0aWZhY3Qx",
        "name": name,
        "size_in_bytes": size,
        "url": "https://api.github.com/repos/Wet-Ink-Corporation/assay/actions/artifacts/1",
        "archive_download_url": "https://api.github.com/repos/Wet-Ink-Corporation/assay/actions/artifacts/1/zip",
        "expired": false,
        "digest": "sha256:00",
        "workflow_run": {
            "id": RUN_ID,
            "repository_id": REPO_ID,
            "head_repository_id": REPO_ID,
            "head_branch": "main",
            "head_sha": "70aa6d748be6f3884e85d294112f9addf83cee59",
        },
    })
}

/// The decide artifact of the pointer's run, `size` bytes zipped and live.
fn decide(size: u64) -> Value {
    artifact(DECIDE, size)
}

/// `artifact` with `field` of its `workflow_run` set to `value`.
fn of_run(mut artifact: Value, field: &str, value: u64) -> Value {
    artifact["workflow_run"][field] = json!(value);
    artifact
}

/// The pointer's run.
fn run_uid() -> RunUid {
    let id = |value| NonZeroU64::new(value).expect("not zero");
    RunUid {
        repo_id: id(REPO_ID),
        run_id: id(RUN_ID),
        run_attempt: id(1),
    }
}

/// An extraction that writes `files` (name and bytes) into the directory.
fn writes(files: &'static [(&'static str, &'static [u8])]) -> Extract {
    Box::new(move |dir| {
        for (name, bytes) in files {
            fs::write(dir.join(name), bytes)?;
        }
        Ok(())
    })
}

/// What the store gives for `file` under `limit`, over `gh`.
fn fetch(
    gh: &ScriptedGh,
    file: &str,
    limit: usize,
) -> Result<Result<StoredBundle, BundleFetchError>, Box<dyn Error>> {
    over(gh, |github| github.fetch(run_uid(), file, limit))
}

/// What the store gives for PR `pr`'s decision bundle under `limit`, over `gh`.
fn fetch_decision(
    gh: &ScriptedGh,
    pr: u64,
    limit: usize,
) -> Result<Result<StoredBundle, BundleFetchError>, Box<dyn Error>> {
    let pr = NonZeroU64::new(pr).ok_or("not zero")?;
    over(gh, |github| github.fetch_decision(run_uid(), pr, limit))
}

/// What `read` gives over the adapter on `gh`, whose transport is then closed.
fn over<T>(
    gh: &ScriptedGh,
    read: impl FnOnce(&GitHub<GhApi<&ScriptedGh>, ThreadSleep>) -> T,
) -> Result<T, Box<dyn Error>> {
    let transport = GhApi::new(
        gh,
        "/usr/bin/gh",
        GhToken::new(TOKEN)?,
        GhEnvironment::default(),
    )?;
    let identity = AppIdentity {
        app: AppId(NonZeroU64::new(5_371_952).ok_or("not zero")?),
        bot: UserId(NonZeroU64::new(2_000_000_001).ok_or("not zero")?),
    };
    let github = GitHub::new(
        Api::new(transport, ThreadSleep),
        "Wet-Ink-Corporation/assay".parse()?,
        identity,
    );
    let fetched = read(&github);
    github.into_api().into_transport().close()?;
    Ok(fetched)
}

#[test]
fn the_decide_artifact_s_bundle_is_downloaded_read_and_its_directory_removed()
-> Result<(), Box<dyn Error>> {
    let gh = ScriptedGh::new(
        &[
            artifact("assay-plan-ffffffffffffffffffffffffffffffff", 10),
            decide(300),
            artifact("coverage", 10),
        ],
        writes(&[(BUNDLE_FILE, b"{\"schema\":\"assay.run-inputs/1\"}")]),
    );

    let fetched = fetch(&gh, BUNDLE_FILE, 1 << 20)?;

    assert_eq!(
        fetched?,
        StoredBundle::Found(b"{\"schema\":\"assay.run-inputs/1\"}".to_vec())
    );
    let calls = gh.calls.borrow();
    let listing = calls.first().ok_or("gh api ran")?;
    assert_eq!(
        listing.last().map(String::as_str),
        Some("repos/Wet-Ink-Corporation/assay/actions/runs/7/artifacts?per_page=100")
    );
    let dirs = gh.dirs.borrow();
    let [dir] = dirs.as_slice() else {
        return Err("one download".into());
    };
    let dir_arg = dir.to_str().ok_or("UTF-8")?;
    assert_eq!(
        gh.downloads(),
        [[
            "run",
            "download",
            "--repo",
            "Wet-Ink-Corporation/assay",
            "--name",
            DECIDE,
            "--dir",
            dir_arg,
            "--",
            "7",
        ]]
    );
    assert_eq!(
        gh.limits.borrow().last().map(|limits| limits.timeout),
        Some(GH_DOWNLOAD_TIMEOUT)
    );
    assert_eq!(*gh.caps.borrow(), [(1 << 20, 16)]);
    assert!(!dir.exists(), "the download's directory is removed");
    Ok(())
}

#[test]
fn a_download_stopped_past_its_directory_cap_is_too_large_and_removed() -> Result<(), Box<dyn Error>>
{
    let gh = ScriptedGh::new(
        &[decide(300)],
        writes(&[(BUNDLE_FILE, b"{\"schema\":\"assay.run-inputs/1\"}")]),
    )
    .killed();

    let fetched = fetch(&gh, BUNDLE_FILE, 8)?;

    assert_eq!(fetched?, StoredBundle::TooLarge);
    assert_eq!(*gh.caps.borrow(), [(8, 16)]);
    assert!(
        gh.dirs.borrow().iter().all(|dir| !dir.exists()),
        "the download's directory is removed"
    );
    Ok(())
}

#[test]
fn no_decide_artifact_is_missing_and_nothing_is_downloaded() -> Result<(), Box<dyn Error>> {
    let gh = ScriptedGh::new(&[artifact("coverage", 10)], writes(&[]));

    let fetched = fetch(&gh, BUNDLE_FILE, 1 << 20)?;

    assert_eq!(fetched?, StoredBundle::Missing);
    assert_eq!(gh.downloads(), Vec::<Vec<String>>::new());
    Ok(())
}

#[test]
fn an_expired_decide_artifact_is_expired_and_not_downloaded() -> Result<(), Box<dyn Error>> {
    let mut expired = decide(300);
    expired["expired"] = json!(true);
    let gh = ScriptedGh::new(&[expired], writes(&[]));

    let fetched = fetch(&gh, BUNDLE_FILE, 1 << 20)?;

    assert_eq!(fetched?, StoredBundle::Expired);
    assert_eq!(gh.downloads(), Vec::<Vec<String>>::new());
    Ok(())
}

#[test]
fn a_zip_too_large_to_hold_the_bundle_is_too_large_and_not_downloaded() -> Result<(), Box<dyn Error>>
{
    let limit: u64 = 1000;
    let room = limit + (1 << 20);
    let at = ScriptedGh::new(&[decide(room)], writes(&[(BUNDLE_FILE, b"{}")]));
    let over = ScriptedGh::new(&[decide(room + 1)], writes(&[]));

    let at_room = fetch(&at, BUNDLE_FILE, 1000)?;
    let past = fetch(&over, BUNDLE_FILE, 1000)?;

    assert_eq!(at_room?, StoredBundle::Found(b"{}".to_vec()));
    assert_eq!(past?, StoredBundle::TooLarge);
    assert_eq!(over.downloads(), Vec::<Vec<String>>::new());
    Ok(())
}

#[test]
fn two_artifacts_named_like_the_decide_job_s_are_ambiguous() -> Result<(), Box<dyn Error>> {
    for second in [
        DECIDE,
        "assay-decide-ffffffffffffffffffffffffffffffff",
        "Assay-Decide-ffffffffffffffffffffffffffffffff",
        "assay-decidex",
    ] {
        let gh = ScriptedGh::new(&[decide(300), artifact(second, 300)], writes(&[]));

        let fetched = fetch(&gh, BUNDLE_FILE, 1 << 20)?;

        assert!(
            matches!(fetched, Err(BundleFetchError::Ambiguous { found: 2 })),
            "{second}: {fetched:?}"
        );
        assert_eq!(gh.downloads(), Vec::<Vec<String>>::new(), "{second}");
    }
    Ok(())
}

#[test]
fn a_decide_artifact_without_a_nonce_is_refused() -> Result<(), Box<dyn Error>> {
    for name in [
        "assay-decide",
        "assay-decide-",
        "assay-decide-0123456789ABCDEF0123456789ABCDEF",
        "assay-decide-0123456789abcdef0123456789abcde",
        "ASSAY-DECIDE-0123456789abcdef0123456789abcdef",
        "assay-decide-0123456789abcdef0123456789abcdef-",
    ] {
        let gh = ScriptedGh::new(&[artifact(name, 300)], writes(&[]));

        let fetched = fetch(&gh, BUNDLE_FILE, 1 << 20)?;

        assert!(
            matches!(fetched, Err(BundleFetchError::Name)),
            "{name}: {fetched:?}"
        );
    }
    Ok(())
}

#[test]
fn a_listing_of_another_run_or_repository_is_refused() -> Result<(), Box<dyn Error>> {
    let mut no_run = decide(300);
    no_run
        .as_object_mut()
        .ok_or("an object")?
        .remove("workflow_run");
    for listed in [
        of_run(decide(300), "id", RUN_ID + 1),
        of_run(decide(300), "repository_id", REPO_ID + 1),
        no_run,
    ] {
        let gh = ScriptedGh::new(std::slice::from_ref(&listed), writes(&[]));

        let fetched = fetch(&gh, BUNDLE_FILE, 1 << 20)?;

        assert!(
            matches!(fetched, Err(BundleFetchError::OtherRun)),
            "{listed}: {fetched:?}"
        );
        assert_eq!(gh.downloads(), Vec::<Vec<String>>::new());
    }
    Ok(())
}

#[test]
fn a_download_holding_anything_but_the_one_file_is_refused() -> Result<(), Box<dyn Error>> {
    let extracts: [(&str, Extract); 4] = [
        (
            "another file beside it",
            writes(&[(BUNDLE_FILE, b"{}"), ("extra.json", b"{}")]),
        ),
        ("only another file", writes(&[("decision.json", b"{}")])),
        (
            "a symlink",
            Box::new(|dir: &Path| symlink("/etc/hostname", dir.join(BUNDLE_FILE))),
        ),
        (
            "a directory",
            Box::new(|dir: &Path| fs::create_dir(dir.join(BUNDLE_FILE))),
        ),
    ];
    for (what, extract) in extracts {
        let gh = ScriptedGh::new(&[decide(300)], extract);

        let fetched = fetch(&gh, BUNDLE_FILE, 1 << 20)?;

        assert!(
            matches!(fetched, Err(BundleFetchError::Contents)),
            "{what}: {fetched:?}"
        );
        let dirs = gh.dirs.borrow();
        assert!(
            dirs.iter().all(|dir| !dir.exists()),
            "{what}: the directory is removed"
        );
    }
    Ok(())
}

#[test]
fn an_empty_download_is_missing() -> Result<(), Box<dyn Error>> {
    let gh = ScriptedGh::new(&[decide(300)], writes(&[]));

    let fetched = fetch(&gh, BUNDLE_FILE, 1 << 20)?;

    assert_eq!(fetched?, StoredBundle::Missing);
    Ok(())
}

#[test]
fn a_bundle_at_the_limit_is_read_and_one_byte_over_is_too_large() -> Result<(), Box<dyn Error>> {
    let at = ScriptedGh::new(&[decide(300)], writes(&[(BUNDLE_FILE, b"12345")]));
    let over = ScriptedGh::new(&[decide(300)], writes(&[(BUNDLE_FILE, b"123456")]));

    let at_limit = fetch(&at, BUNDLE_FILE, 5)?;
    let past = fetch(&over, BUNDLE_FILE, 5)?;

    assert_eq!(at_limit?, StoredBundle::Found(b"12345".to_vec()));
    assert_eq!(past?, StoredBundle::TooLarge);
    Ok(())
}

#[test]
fn a_failed_download_is_reported_without_the_token() -> Result<(), Box<dyn Error>> {
    let gh = ScriptedGh::new(&[decide(300)], writes(&[])).failing(
        1,
        format!("error downloading {DECIDE} with {TOKEN}: 410 Gone").as_bytes(),
    );

    let fetched = fetch(&gh, BUNDLE_FILE, 1 << 20)?;

    let Err(BundleFetchError::Download(GhDownloadError::Failed { exit, stderr })) = fetched else {
        return Err(format!("a failed download, got {fetched:?}").into());
    };
    assert_eq!(exit.code(), Some(1));
    assert_eq!(
        stderr,
        format!("error downloading {DECIDE} with <redacted>: 410 Gone")
    );
    assert!(
        gh.dirs.borrow().iter().all(|dir| !dir.exists()),
        "the directory is removed"
    );
    Ok(())
}

// --- decide-only's decision bundles (D5): `assay-decision-<pr>-<nonce>`, one per PR ---

/// The PR the decision bundle is asked for.
const DECIDED_PR: u64 = 352;
/// The decision artifact of [`DECIDED_PR`], as decide-only names it.
const DECISION: &str = "assay-decision-352-fedcba9876543210fedcba9876543210";

/// The decision artifact of [`DECIDED_PR`], `size` bytes zipped and live.
fn decision(size: u64) -> Value {
    artifact(DECISION, size)
}

#[test]
fn the_pr_s_decision_artifact_is_the_one_downloaded_among_the_run_s_others()
-> Result<(), Box<dyn Error>> {
    let gh = ScriptedGh::new(
        &[
            artifact("assay-decision-351-fedcba9876543210fedcba9876543210", 300),
            artifact("assay-decision-3520-fedcba9876543210fedcba9876543210", 300),
            decision(300),
            decide(300),
        ],
        writes(&[(DECISION_FILE, b"{\"schema\":\"assay.run-decision/1\"}")]),
    );

    let fetched = fetch_decision(&gh, DECIDED_PR, MAX_DECISION_BUNDLE_BYTES)?;

    assert_eq!(
        fetched?,
        StoredBundle::Found(b"{\"schema\":\"assay.run-decision/1\"}".to_vec())
    );
    let downloads = gh.downloads();
    let [download] = downloads.as_slice() else {
        return Err(format!("one download, got {downloads:?}").into());
    };
    let name = download
        .iter()
        .position(|arg| arg == "--name")
        .and_then(|at| download.get(at + 1));
    assert_eq!(name.map(String::as_str), Some(DECISION));
    assert_eq!(download.last().map(String::as_str), Some("7"));
    assert_eq!(*gh.caps.borrow(), [(8 << 20, 16)]);
    Ok(())
}

#[test]
fn only_another_pr_s_decision_artifact_is_missing_and_nothing_is_downloaded()
-> Result<(), Box<dyn Error>> {
    let gh = ScriptedGh::new(
        &[
            artifact("assay-decision-351-fedcba9876543210fedcba9876543210", 300),
            artifact("assay-decision-35-fedcba9876543210fedcba9876543210", 300),
            decide(300),
        ],
        writes(&[]),
    );

    let fetched = fetch_decision(&gh, DECIDED_PR, 1 << 20)?;

    assert_eq!(fetched?, StoredBundle::Missing);
    assert_eq!(gh.downloads(), Vec::<Vec<String>>::new());
    Ok(())
}

#[test]
fn two_decision_artifacts_for_one_pr_are_ambiguous() -> Result<(), Box<dyn Error>> {
    for second in [
        DECISION,
        "assay-decision-352-0123456789abcdef0123456789abcdef",
        "Assay-Decision-352-0123456789abcdef0123456789abcdef",
        "assay-decision-352-",
    ] {
        let gh = ScriptedGh::new(&[decision(300), artifact(second, 300)], writes(&[]));

        let fetched = fetch_decision(&gh, DECIDED_PR, 1 << 20)?;

        assert!(
            matches!(fetched, Err(BundleFetchError::Ambiguous { found: 2 })),
            "{second}: {fetched:?}"
        );
        assert_eq!(gh.downloads(), Vec::<Vec<String>>::new(), "{second}");
    }
    Ok(())
}

#[test]
fn a_decision_artifact_of_the_pr_without_a_nonce_is_refused() -> Result<(), Box<dyn Error>> {
    for name in [
        "assay-decision-352-",
        "assay-decision-352-FEDCBA9876543210FEDCBA9876543210",
        "ASSAY-DECISION-352-fedcba9876543210fedcba9876543210",
        "assay-decision-352-fedcba9876543210fedcba9876543210-",
    ] {
        let gh = ScriptedGh::new(&[artifact(name, 300)], writes(&[]));

        let fetched = fetch_decision(&gh, DECIDED_PR, 1 << 20)?;

        assert!(
            matches!(fetched, Err(BundleFetchError::Name)),
            "{name}: {fetched:?}"
        );
    }
    Ok(())
}

#[test]
fn a_decision_artifact_listed_for_another_run_is_refused_and_an_expired_one_is_expired()
-> Result<(), Box<dyn Error>> {
    let other = ScriptedGh::new(&[of_run(decision(300), "id", RUN_ID + 1)], writes(&[]));
    let mut expired = decision(300);
    expired["expired"] = json!(true);
    let gone = ScriptedGh::new(&[expired], writes(&[]));

    let other_run = fetch_decision(&other, DECIDED_PR, 1 << 20)?;
    let past_retention = fetch_decision(&gone, DECIDED_PR, 1 << 20)?;

    assert!(
        matches!(other_run, Err(BundleFetchError::OtherRun)),
        "{other_run:?}"
    );
    assert_eq!(past_retention?, StoredBundle::Expired);
    Ok(())
}

#[test]
fn a_decision_download_holding_the_evidence_file_instead_is_refused() -> Result<(), Box<dyn Error>>
{
    let gh = ScriptedGh::new(&[decision(300)], writes(&[(BUNDLE_FILE, b"{}")]));

    let fetched = fetch_decision(&gh, DECIDED_PR, 1 << 20)?;

    assert!(
        matches!(fetched, Err(BundleFetchError::Contents)),
        "{fetched:?}"
    );
    Ok(())
}
