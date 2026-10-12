//! In-memory fakes of the `plan` ports (TST-09): a PR's base and head trees with its diff, and
//! a deterministic nonce source.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::num::NonZeroU64;

use assay_app::{
    ArtifactNonce, ChangedFiles, CommitId, EntryKind, IncludeTargets, NonceSource, PackageSet,
    PlanRequest, PullRequestEvent, PullRequestObjects, RestFile, RestFiles, TreeEntry,
};
use assay_domain::{ContentHash, FileChange, Lines, RunUid};

pub(crate) const BASE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub(crate) const HEAD: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
pub(crate) const PIN: &str = "0123456789abcdef0123456789abcdef01234567";
pub(crate) const DIGEST: &str =
    "sha256:dc7c1d8e3ccab68cfdb828ce40a446c3913f7172f899ffe3f439839cfd917a8b";

/// A base `.assay/plugins.toml` with a required temper gate (one lint step) and an optional
/// `exec` gate in the test job.
pub(crate) fn plugins_toml() -> String {
    format!(
        r#"
[assay]
digest = "{DIGEST}"
claude_cli = "2.1.296"

[[plugin]]
name = "temper"
adapter = "builtin:temper"
pin = "git:{PIN}"
required = true

[[plugin.step]]
name = "fmt"
job = "lint"
cmd = ["cargo", "fmt", "--check"]
timeout_s = 300

[[plugin]]
name = "custom-lint"
adapter = "exec"
exec = ["./scripts/lint.sh"]
job = "test"
required = false
"#
    )
}

/// What the fake's port reports when asked something it was told to fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FakeError;

impl fmt::Display for FakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the fake was told to fail")
    }
}

impl Error for FakeError {}

/// One file of a fake tree.
#[derive(Debug, Clone)]
pub(crate) struct File {
    kind: EntryKind,
    bytes: Vec<u8>,
    /// The size the listing reports, when it is not the blob's.
    listed: Option<u64>,
}

/// A PR as data: two trees, keyed by commit, and the diff between them.
#[derive(Debug, Clone)]
pub(crate) struct FakePr {
    trees: BTreeMap<String, BTreeMap<String, File>>,
    /// The head commit `changed_files` answers for.
    head: String,
    pub(crate) changes: ChangedFiles,
    /// Reads return one blob fewer than asked for.
    pub(crate) short_reads: bool,
    /// Every read fails.
    pub(crate) failing_reads: bool,
}

impl FakePr {
    /// Base and head both hold `plugins.toml` and a crate; the head changes `src/lib.rs`.
    pub(crate) fn new() -> Self {
        let tree: BTreeMap<String, File> = [
            (".assay/plugins.toml", plugins_toml()),
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.1.0\"\n".to_owned(),
            ),
            ("README.md", "# x\n".to_owned()),
            ("src/lib.rs", "pub fn one() {}\n".to_owned()),
        ]
        .into_iter()
        .map(|(path, text)| (path.to_owned(), regular(text.as_bytes())))
        .collect();
        let mut pr = Self {
            trees: [(BASE.to_owned(), tree.clone()), (HEAD.to_owned(), tree)].into(),
            head: HEAD.to_owned(),
            changes: ChangedFiles::Listed(Vec::new()),
            short_reads: false,
            failing_reads: false,
        };
        pr.write(HEAD, "src/lib.rs", b"pub fn two() {}\n");
        pr.changes = ChangedFiles::Listed(vec![FileChange::new(
            "src/lib.rs",
            Lines::Counted {
                added: 1,
                removed: 1,
            },
        )]);
        pr
    }

    /// The same PR with its head tree and diff under the commit id `head`.
    pub(crate) fn at_head(&self, head: &str) -> Self {
        let mut moved = self.clone();
        if let Some(tree) = moved.trees.remove(&self.head) {
            moved.trees.insert(head.to_owned(), tree);
        }
        head.clone_into(&mut moved.head);
        moved
    }

    /// Writes a regular file at `path` in `commit`'s tree.
    pub(crate) fn write(&mut self, commit: &str, path: &str, bytes: &[u8]) {
        self.put(commit, path, regular(bytes));
    }

    /// Writes a regular file at `path` in `commit`'s tree whose listing reports `size` bytes.
    pub(crate) fn write_listed_as(
        &mut self,
        (commit, path): (&str, &str),
        bytes: &[u8],
        size: u64,
    ) {
        self.put(
            commit,
            path,
            File {
                listed: Some(size),
                ..regular(bytes)
            },
        );
    }

    /// Puts a symlink to `target` at `path` in `commit`'s tree.
    pub(crate) fn symlink(&mut self, commit: &str, path: &str, target: &[u8]) {
        self.put(
            commit,
            path,
            File {
                kind: EntryKind::Symlink,
                bytes: target.to_vec(),
                listed: None,
            },
        );
    }

    /// Puts a submodule at `path` in `commit`'s tree.
    pub(crate) fn submodule(&mut self, commit: &str, path: &str) {
        self.put(
            commit,
            path,
            File {
                kind: EntryKind::Submodule,
                bytes: Vec::new(),
                listed: None,
            },
        );
    }

    /// Removes `path` from `commit`'s tree.
    pub(crate) fn remove(&mut self, commit: &str, path: &str) {
        if let Some(tree) = self.trees.get_mut(commit) {
            tree.remove(path);
        }
    }

    /// Drops `commit`'s tree, so listing it fails.
    pub(crate) fn forget(&mut self, commit: &str) {
        self.trees.remove(commit);
    }

    fn put(&mut self, commit: &str, path: &str, file: File) {
        self.trees
            .entry(commit.to_owned())
            .or_default()
            .insert(path.to_owned(), file);
    }

    /// The listed files, with the REST list GitHub would report for them.
    pub(crate) fn rest_files(&self) -> RestFiles {
        let files: Vec<RestFile> = match &self.changes {
            ChangedFiles::Listed(files) => files
                .iter()
                .map(|file| RestFile {
                    path: file.path.clone(),
                    previous_path: file.renamed_from.clone(),
                })
                .collect(),
            ChangedFiles::TooSlow => Vec::new(),
        };
        RestFiles {
            changed_files: u64::try_from(files.len()).unwrap_or(u64::MAX),
            files,
        }
    }
}

fn regular(bytes: &[u8]) -> File {
    File {
        kind: EntryKind::File,
        bytes: bytes.to_vec(),
        listed: None,
    }
}

impl PullRequestObjects for FakePr {
    type Error = FakeError;

    fn changed_files(&self, base: &CommitId, head: &CommitId) -> Result<ChangedFiles, FakeError> {
        if (base.as_str(), head.as_str()) == (BASE, self.head.as_str()) {
            Ok(self.changes.clone())
        } else {
            Err(FakeError)
        }
    }

    fn list(&self, commit: &CommitId, path: Option<&str>) -> Result<Vec<TreeEntry>, FakeError> {
        let tree = self.trees.get(commit.as_str()).ok_or(FakeError)?;
        Ok(tree
            .iter()
            .filter(|(entry, _)| {
                path.is_none_or(|path| {
                    *entry == path
                        || entry
                            .strip_prefix(path)
                            .is_some_and(|rest| rest.starts_with('/'))
                })
            })
            .map(|(entry, file)| TreeEntry {
                path: entry.clone(),
                kind: file.kind,
                object: format!("{}:{entry}", commit.as_str()),
                size: (file.kind != EntryKind::Submodule).then(|| {
                    file.listed
                        .unwrap_or_else(|| u64::try_from(file.bytes.len()).unwrap_or(u64::MAX))
                }),
            })
            .collect())
    }

    fn read(&self, entries: &[&TreeEntry]) -> Result<Vec<Vec<u8>>, FakeError> {
        if self.failing_reads {
            return Err(FakeError);
        }
        let mut blobs = entries
            .iter()
            .map(|entry| {
                let (commit, path) = entry.object.split_once(':').ok_or(FakeError)?;
                let file = self
                    .trees
                    .get(commit)
                    .and_then(|tree| tree.get(path))
                    .filter(|file| file.kind != EntryKind::Submodule)
                    .ok_or(FakeError)?;
                Ok(file.bytes.clone())
            })
            .collect::<Result<Vec<_>, FakeError>>()?;
        if self.short_reads {
            blobs.pop();
        }
        Ok(blobs)
    }
}

/// Nonces `0..`, as 32 hex digits; or always the same one.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CountingNonces {
    pub(crate) next: u128,
    pub(crate) stuck: bool,
}

impl NonceSource for CountingNonces {
    type Error = FakeError;

    fn nonce(&mut self) -> Result<ArtifactNonce, FakeError> {
        let nonce = format!("{:032x}", self.next)
            .parse()
            .map_err(|_unparsed| FakeError)?;
        if !self.stuck {
            self.next = self.next.saturating_add(1);
        }
        Ok(nonce)
    }
}

/// A request for the fake PR: PR 42 into `main`, not a draft, its REST list matching git, an
/// empty package set on both sides and no compile-time include.
pub(crate) fn request(pr: &FakePr) -> Result<PlanRequest, Box<dyn Error>> {
    Ok(PlanRequest {
        pull_request: PullRequestEvent {
            repo: "Wet-Ink-Corporation/assay".to_owned(),
            number: NonZeroU64::new(42).ok_or("42 is not zero")?,
            base_sha: BASE.parse()?,
            head_sha: HEAD.parse()?,
            base_ref: "main".to_owned(),
            draft: false,
        },
        run_uid: run_uid(1)?,
        rest_files: pr.rest_files(),
        base_packages: Some(PackageSet::default()),
        head_packages: Some(PackageSet::default()),
        include_targets: IncludeTargets::Scanned(Vec::new()),
        prompt_bundle: ContentHash::of_files([("prompt.md", b"review".as_slice())]),
    })
}

/// Run 7 of repository 840213377, at `attempt`.
pub(crate) fn run_uid(attempt: u64) -> Result<RunUid, Box<dyn Error>> {
    Ok(RunUid {
        repo_id: NonZeroU64::new(840_213_377).ok_or("not zero")?,
        run_id: NonZeroU64::new(7).ok_or("not zero")?,
        run_attempt: NonZeroU64::new(attempt).ok_or("attempts count from 1")?,
    })
}
