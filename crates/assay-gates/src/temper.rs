//! The `builtin:temper` adapter in `gates-static`, zone Z2s: the decision-relevant temper checks
//! that need no compilation, out of a hostile `build.rs`'s reach
//! (`docs/architecture/03-contracts.md` §2.1 steps 1-3, ADR-12, 05 T4 and T5).
//!
//! 1. `raised`: `.claude/temper-baseline.json` at base and head; any count that rises, any new
//!    entry, and any crate added to or moved in `crates` fails.
//! 2. `exception`: the base's pinned temper runs `node <pinned>/temper.mjs scan --all --json`
//!    over the head tree; every `ask` hit not admitted by base `.assay/exceptions.toml` fails,
//!    as does an `#[expect]` naming more than one lint. What the scan cannot see (Rust under
//!    the directories it skips, submodules, unsafe symlinks, `cfg_attr` suppressions and
//!    `#[path]`) is a `blind-spot` that fails too.
//! 3. Gate-config diff: the keys of `.claude/temper.json` that steer temper, at base and head;
//!    any difference fails unless base admits that exact base and head value.
//!
//! The PR's vendored `.claude/temper/` is never run; its drift from the pinned copy is a
//! `major` finding. A file the checks need that is missing or unparsable, a scan that does not
//! exit 0 or 1, or an unreadable pinned copy is a [`TemperError`]: the gate errors, it never
//! passes.

mod attributes;
mod baseline;
#[cfg(test)]
mod corpus;
mod error;
mod exceptions;
mod findings;
mod gate_config;
mod head;
mod references;
mod report;
mod scan;
mod tokens;
mod vendored;

use std::env;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use assay_app::{Adapter, GateDocument, GateJob, GateRun, GateRunner, Plugin, Termination};
use assay_domain::{Finding, FindingsDocument, FindingsSchema, GateConclusion, Source, SourceKind};

pub use error::{TemperError, TemperSettingsError};

use self::baseline::Totals;
use self::exceptions::Allowlist;
use self::gate_config::TemperConfig;
use self::head::HeadTree;
use self::vendored::PinnedIds;
use crate::capture::CaptureSupervisor;
use crate::environment::GateEnvironment;
use crate::exec::ProcessSpec;
use crate::revision::{EntryKind, Revision, RevisionFiles, TreeEntry};

/// The pinned copy's entry point, relative to its root.
const SCRIPT: &str = "temper.mjs";

/// The scan's arguments after the script: a pure source scan of every `.rs` file, as JSON.
const SCAN: [&str; 3] = ["scan", "--all", "--json"];

/// Most bytes of scan output read (and, separately, of its stderr).
const MAX_SCAN_OUTPUT_BYTES: usize = 16 << 20;

/// How [`TemperError::Revision`] names the whole tree.
const TREE: &str = ".";

/// Where the temper adapter's programs and trees are in this job. Every path is absolute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemperPaths {
    /// The `node` binary that runs the pinned temper; never looked up on `PATH`.
    pub node: PathBuf,
    /// The pinned temper copy, in its vendored layout (`temper.mjs`, `lib/`, `VERSION`),
    /// prepared from the base's pin.
    pub pinned: PathBuf,
    /// The head commit's tree on disk, which the scan reads.
    pub head_tree: PathBuf,
}

/// Where and how the temper adapter runs in this job.
#[derive(Debug, Clone)]
pub struct TemperSettings {
    node: PathBuf,
    pinned: PathBuf,
    head_tree: PathBuf,
    scan_args: Vec<String>,
    scan_env: Vec<(OsString, OsString)>,
    run_key: String,
}

impl TemperSettings {
    /// Settings that run `node <pinned>/temper.mjs scan --all --json` from the head tree, with
    /// `environment` plus git hardening, and write documents for run `run_key`.
    ///
    /// The scan also sees `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL=/dev/null` and
    /// `GIT_CEILING_DIRECTORIES` set to the head tree's parent, so temper's project discovery
    /// cannot find a repository above the tree. The head tree should not sit inside a Cargo
    /// workspace either, since temper falls back to the outermost `Cargo.toml` above it.
    ///
    /// # Errors
    ///
    /// [`TemperSettingsError`] when `node` is relative; the pinned copy is relative or not
    /// UTF-8; the head tree is relative or the root; `environment`'s `PATH` has an empty or
    /// relative entry, which temper's own `git` would resolve from the head tree; or `run_key`
    /// is empty.
    pub fn new(
        paths: TemperPaths,
        environment: &GateEnvironment,
        run_key: impl Into<String>,
    ) -> Result<Self, TemperSettingsError> {
        let TemperPaths {
            node,
            pinned,
            head_tree,
        } = paths;
        if !node.is_absolute() {
            return Err(TemperSettingsError::RelativeNode { path: node });
        }
        if !pinned.is_absolute() {
            return Err(TemperSettingsError::RelativePinned { path: pinned });
        }
        let Some(script) = pinned.join(SCRIPT).to_str().map(str::to_owned) else {
            return Err(TemperSettingsError::NonUtf8Pinned { path: pinned });
        };
        let ceiling = match head_tree.parent() {
            Some(parent) if head_tree.is_absolute() => parent.as_os_str().to_owned(),
            _ => return Err(TemperSettingsError::HeadTree { path: head_tree }),
        };
        if !search_path_is_absolute(environment) {
            return Err(TemperSettingsError::RelativeSearchPath);
        }
        let run_key = run_key.into();
        if run_key.is_empty() {
            return Err(TemperSettingsError::EmptyRunKey);
        }
        let mut scan_env = environment.vars().to_vec();
        scan_env.extend([
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ("GIT_CEILING_DIRECTORIES".into(), ceiling),
        ]);
        let scan_args = [script]
            .into_iter()
            .chain(SCAN.map(str::to_owned))
            .collect();
        Ok(Self {
            node,
            pinned,
            head_tree,
            scan_args,
            scan_env,
            run_key,
        })
    }
}

/// Whether every entry of `environment`'s `PATH` is an absolute directory. An empty entry
/// means the working directory, here the head tree.
fn search_path_is_absolute(environment: &GateEnvironment) -> bool {
    environment
        .vars()
        .iter()
        .filter(|(name, _)| name == "PATH")
        .all(|(_, value)| env::split_paths(value).all(|entry| entry.is_absolute()))
}

/// The temper adapter, generic over the supervisor that runs the pinned scan and the reader of
/// the PR's base and head trees.
#[derive(Debug)]
pub struct TemperAdapter<S, R> {
    supervisor: S,
    revisions: R,
    settings: TemperSettings,
}

impl<S, R> TemperAdapter<S, R> {
    /// An adapter that runs the scan through `supervisor` and reads the PR through
    /// `revisions`. Pass `&mut supervisor` and `&revisions` to keep them.
    pub const fn new(supervisor: S, revisions: R, settings: TemperSettings) -> Self {
        Self {
            supervisor,
            revisions,
            settings,
        }
    }
}

/// The adapter's error for a supervisor `S` and a reader `R`.
type ErrorOf<S, R> = TemperError<<S as CaptureSupervisor>::Error, <R as RevisionFiles>::Error>;

impl<S: CaptureSupervisor, R: RevisionFiles> GateRunner for TemperAdapter<S, R> {
    type Error = ErrorOf<S, R>;

    /// Runs the Z2s checks and reports them as one `assay.findings/1` document that concludes
    /// `fail` when `raised`, `exception`, a blind spot or the gate-config diff found anything.
    ///
    /// # Errors
    ///
    /// [`TemperError`] without running anything when `plugin` is not temper or `job` is not
    /// `gates-static`; and when a file the checks need is missing, not a regular file or
    /// unparsable, the head tree cannot be read, the scan cannot run, does not exit 0 or 1 or
    /// prints unreadable output, or the pinned copy cannot be read.
    fn run(&mut self, plugin: &Plugin, job: GateJob) -> Result<GateRun, Self::Error> {
        let Adapter::Temper { pin, .. } = plugin.adapter() else {
            return Err(TemperError::NotTemper {
                plugin: plugin.name().as_str().to_owned(),
            });
        };
        if job != GateJob::Static {
            return Err(TemperError::OutsideStatic { job });
        }
        let pin = pin.to_string();
        let allowlist = self.allowlist()?;
        let mut failing = self.raised()?;
        failing.extend(self.gate_config(&allowlist)?);
        let head = self.head_tree()?;
        let analysis = head.analyse();
        failing.extend(findings::blind(&analysis.blind));
        let requests = self.scan(plugin.timeout())?;
        failing.extend(findings::exceptions(requests, &analysis, &allowlist));
        let drift = self.vendored_drift(&head, &pin)?;
        let conclusion = if failing.is_empty() {
            GateConclusion::Pass
        } else {
            GateConclusion::Fail
        };
        failing.extend(drift);
        let document = FindingsDocument {
            schema: FindingsSchema::V1,
            source: Source {
                kind: SourceKind::Gate,
                name: plugin.name().as_str().to_owned(),
                version: pin,
                role: None,
                // The settings keep the key for the next run; the document owns its copy.
                run_key: self.settings.run_key.clone(),
            },
            conclusion: Some(conclusion),
            findings: failing,
            stats: None,
        };
        let bytes = serde_json::to_vec(&document).map_err(TemperError::Document)?;
        Ok(GateRun {
            termination: Termination::Exited(0),
            document: GateDocument::Written(bytes),
        })
    }
}

impl<S: CaptureSupervisor, R: RevisionFiles> TemperAdapter<S, R> {
    /// Step 1: the baseline diff, counts and crates.
    fn raised(&self) -> Result<Vec<Finding>, ErrorOf<S, R>> {
        let base = self.required(Revision::Base, baseline::PATH)?;
        let head = self.required(Revision::Head, baseline::PATH)?;
        let parse = |revision, bytes: &[u8]| {
            Totals::parse(bytes).map_err(|detail| malformed(revision, baseline::PATH, detail))
        };
        let (before, after) = (parse(Revision::Base, &base)?, parse(Revision::Head, &head)?);
        let text = located_text(&head);
        let mut found = findings::raised(&baseline::raises(&before, &after), text);
        found.extend(findings::moved_crates(
            &baseline::moved_crates(&before, &after),
            text,
        ));
        Ok(found)
    }

    /// Base `.assay/exceptions.toml`; an absent file admits nothing.
    fn allowlist(&self) -> Result<Allowlist, ErrorOf<S, R>> {
        let Some(bytes) = self.read(Revision::Base, exceptions::PATH)? else {
            return Ok(Allowlist::default());
        };
        str::from_utf8(&bytes)
            .map_err(|_utf8| "not UTF-8".to_owned())
            .and_then(Allowlist::parse)
            .map_err(|detail| malformed(Revision::Base, exceptions::PATH, detail))
    }

    /// Step 3: the gate-config diff.
    fn gate_config(&self, allowlist: &Allowlist) -> Result<Vec<Finding>, ErrorOf<S, R>> {
        let base = self.required(Revision::Base, gate_config::PATH)?;
        let head = self.required(Revision::Head, gate_config::PATH)?;
        let parse = |revision, bytes: &[u8]| {
            TemperConfig::parse(bytes)
                .map_err(|detail| malformed(revision, gate_config::PATH, detail))
        };
        let (before, after) = (parse(Revision::Base, &base)?, parse(Revision::Head, &head)?);
        let drifted = gate_config::drifted(&before, &after, allowlist);
        Ok(findings::gate_config(
            &drifted,
            str::from_utf8(&base).ok(),
            str::from_utf8(&head).ok(),
        ))
    }

    /// The head tree, with the bytes of its `.rs` files and symlinks, in one bounded read.
    fn head_tree(&self) -> Result<HeadTree, ErrorOf<S, R>> {
        let failed = |source| TemperError::Revision {
            revision: Revision::Head,
            path: TREE,
            source,
        };
        let entries = self.revisions.list(Revision::Head, None).map_err(failed)?;
        let wanted: Vec<&TreeEntry> = entries
            .iter()
            .filter(|entry| head::needs_bytes(entry))
            .collect();
        let blobs = self.revisions.read(&wanted).map_err(failed)?;
        Ok(HeadTree::new(entries, blobs))
    }

    /// Step 2: the exception requests of the pinned scan over the head tree.
    fn scan(&mut self, timeout: Duration) -> Result<Vec<scan::Request>, ErrorOf<S, R>> {
        let spec = ProcessSpec {
            program: &self.settings.node,
            args: &self.settings.scan_args,
            cwd: &self.settings.head_tree,
            env: &self.settings.scan_env,
            timeout,
            max_output_bytes: MAX_SCAN_OUTPUT_BYTES,
        };
        let run = self
            .supervisor
            .capture(&spec)
            .map_err(TemperError::Supervisor)?;
        match run.termination {
            Termination::Exited(0 | 1) => {}
            termination => return Err(TemperError::ScanEnded { termination }),
        }
        scan::requests(&run.stdout).map_err(|detail| TemperError::ScanOutput { detail })
    }

    /// The vendored copy's drift from the pinned copy, compared by blob id.
    fn vendored_drift(&self, head: &HeadTree, pin: &str) -> Result<Option<Finding>, ErrorOf<S, R>> {
        let pinned = vendored::read_pinned(&self.settings.pinned)
            .map_err(|(path, source)| TemperError::Pinned { path, source })?;
        let ids = pinned
            .into_iter()
            .map(|(path, bytes)| Ok((path, self.revisions.blob_id(&bytes)?)))
            .collect::<Result<PinnedIds, _>>()
            .map_err(TemperError::BlobId)?;
        Ok(findings::drift(&vendored::drift(&ids, head.entries()), pin))
    }

    /// The bytes of the regular file at `path` in `revision`, or an error when it is absent.
    fn required(&self, revision: Revision, path: &'static str) -> Result<Vec<u8>, ErrorOf<S, R>> {
        self.read(revision, path)?
            .ok_or(TemperError::Missing { revision, path })
    }

    /// The bytes of the regular file at `path` in `revision`, `None` when nothing is there, and
    /// an error when something other than one regular file is.
    fn read(
        &self,
        revision: Revision,
        path: &'static str,
    ) -> Result<Option<Vec<u8>>, ErrorOf<S, R>> {
        let failed = |source| TemperError::Revision {
            revision,
            path,
            source,
        };
        let entries = self.revisions.list(revision, Some(path)).map_err(failed)?;
        let entry = match entries.as_slice() {
            [] => return Ok(None),
            [entry] if entry.path == path && entry.kind == EntryKind::File => entry,
            _ => return Err(TemperError::NotAFile { revision, path }),
        };
        let blobs = self.revisions.read(&[entry]).map_err(failed)?;
        blobs
            .into_iter()
            .next()
            .map(Some)
            .ok_or(TemperError::Missing { revision, path })
    }
}

/// `bytes` as text to locate findings in. Text that is not UTF-8 only costs the findings
/// their line: they fall back to a synthesised quote at line 1.
fn located_text(bytes: &[u8]) -> Option<&str> {
    str::from_utf8(bytes).ok()
}

/// A [`TemperError::Malformed`] for `path` at `revision`.
const fn malformed<S, R>(
    revision: Revision,
    path: &'static str,
    detail: String,
) -> TemperError<S, R> {
    TemperError::Malformed {
        revision,
        path,
        detail,
    }
}
