//! The `builtin:temper` adapter. In `gates-static`, zone Z2s: the decision-relevant temper
//! checks that need no compilation, out of a hostile `build.rs`'s reach
//! (`docs/architecture/03-contracts.md` §2.1 steps 1-4, ADR-12, 05 T4 and T5). In `gates-lint`
//! (Z2b): the pinned `baseline --check` with its ratchet asserted, and the declared `lint`
//! steps (steps 5-7, `temper/lint.rs`). In `gates-test` (Z2b, its own VM): the declared `test`
//! steps (step 7, `temper/test_job.rs`). A declared step of either job is retried once when a
//! signal ended it (F9, `temper/steps.rs`).
//!
//! 1. `raised`: `.claude/temper-baseline.json` at base and head; any count that rises, any new
//!    entry, and any crate added to or moved in `crates` fails.
//! 2. `exception`: the base's pinned temper runs `node <pinned>/temper.mjs scan --all --json`
//!    over the head tree; every `ask` hit not admitted by base `.assay/exceptions.toml` fails,
//!    as does an `#[expect]` naming more than one lint. What the scan cannot see (Rust under
//!    the directories it skips, submodules, symlinks, `cfg_attr` suppressions, `#[path]`,
//!    `include!`, modules without their files, a plain `allow` it did not report, and a Cargo
//!    build script or target `path` that is not a `.rs` file of the tree) is a `blind-spot`
//!    that fails too.
//! 3. Gate-config diff: the keys of `.claude/temper.json` that steer temper, at base and head;
//!    any difference fails unless base admits that exact base and head value.
//!
//! The PR's vendored `.claude/temper/` is never run; its drift from the pinned copy is a
//! `major` finding. A file the checks need that is missing or unparsable, a scan that does not
//! exit 0 or 1, or an unreadable pinned copy is a [`TemperError`]: the gate errors, it never
//! passes.

mod attributes;
mod baseline;
mod cargo_targets;
mod closure;
#[cfg(test)]
mod corpus;
mod error;
mod evidence;
mod exceptions;
mod findings;
mod gate_config;
mod head;
mod inventory;
mod layout;
mod lint;
mod lint_findings;
mod manifests;
mod ratchet;
mod reach;
mod references;
mod report;
mod scan;
mod static_findings;
mod statics;
mod steps;
mod switches;
mod test_job;
mod tokens;
mod vendored;

use std::path::PathBuf;
use std::time::Duration;

use assay_app::{
    Adapter, BuildJob, CommitId, GateError, GateJob, GateRun, GateRunner, Plugin, Termination,
};
use assay_domain::{Finding, GateConclusion};

pub use closure::{CompileTimeReach, compile_time_reach};
pub use error::{TemperError, TemperSettingsError};
pub use ratchet::RatchetDegradation;
pub use steps::TemperStepSettings;
pub use test_job::TemperTestAdapter;

use self::baseline::Totals;
use self::exceptions::Allowlist;
use self::gate_config::TemperConfig;
use self::head::HeadTree;
use self::statics::{Files, Tree};
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

/// The commits `gates-lint`'s ratchet is checked against (03 §2.1 step 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RatchetBase {
    /// The PR's base commit, passed to temper as `--base` (never a moving `origin/<x>` ref).
    pub base: CommitId,
    /// The merge base of the base and head commits, computed by assay in Z2s; temper's
    /// `ratchet.base` must equal it.
    pub merge_base: CommitId,
}

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
    script: String,
    scan_args: Vec<String>,
    /// The head tree, the environment (the scan's and every declared step's) and the run key.
    steps: TemperStepSettings,
    ratchet: Option<RatchetBase>,
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
        let steps = TemperStepSettings::new(head_tree, environment, run_key)?;
        // The scan's argv keeps its own copy of the script path; `gates-lint` builds its argv
        // from `script`.
        let scan_args = [script.clone()]
            .into_iter()
            .chain(SCAN.map(str::to_owned))
            .collect();
        Ok(Self {
            node,
            pinned,
            script,
            scan_args,
            steps,
            ratchet: None,
        })
    }

    /// These settings, able to run `gates-lint`: temper's ratchet runs against `ratchet.base`
    /// and is believed only when its base is `ratchet.merge_base`. Without it, `gates-lint`
    /// is [`TemperError::NoRatchetBase`].
    #[must_use]
    pub fn with_ratchet(self, ratchet: RatchetBase) -> Self {
        Self {
            ratchet: Some(ratchet),
            ..self
        }
    }

    /// `<pinned>/temper.mjs baseline --check --locked --base <base> --findings-out <out>`, when
    /// a ratchet base is set.
    fn lint_args(&self) -> Option<Vec<String>> {
        let base = self.ratchet.as_ref()?.base.as_str();
        let args = [
            &self.script,
            "baseline",
            "--check",
            "--locked",
            "--base",
            base,
            "--findings-out",
            lint::FINDINGS_OUT,
        ];
        Some(args.map(str::to_owned).to_vec())
    }
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

    /// In `gates-static`, runs the Z2s checks and reports them as one `assay.findings/1`
    /// document that concludes `fail` when `raised`, `exception`, a blind spot, the gate-config
    /// diff or a step 4 check found anything. In `gates-lint`, runs the pinned `baseline
    /// --check` and the declared `lint` steps, and concludes `fail` when the ratchet fails, a
    /// `raised` or unadmitted `exception` entry is reported, or a step exits non-zero; a run
    /// that does not exit, or a `baseline --check` exit other than 0 or 1, is reported as that
    /// termination. In `gates-test`, runs the declared `test` steps and concludes `fail` when one
    /// exits non-zero; a step that does not exit is reported as that termination. A declared
    /// step of either job that a signal ended is run once more, and that run decides.
    ///
    /// # Errors
    ///
    /// [`TemperError`] without running anything when `plugin` is not temper, `job` is
    /// `gates-test` and the plugin declares no `test` step, or `gates-lint` has no
    /// [`RatchetBase`]; when a file the checks need is missing, not a regular file or
    /// unparsable, the head tree cannot be read, a process cannot run, the scan does not exit 0
    /// or 1 or prints unreadable output, or the pinned copy cannot be read; and in `gates-lint`
    /// when temper's findings document is unusable or its ratchet is degraded
    /// ([`TemperError::RatchetDegraded`]).
    fn run(&mut self, plugin: &Plugin, job: GateJob) -> Result<GateRun, Self::Error> {
        let Adapter::Temper { pin, steps } = plugin.adapter() else {
            return Err(TemperError::NotTemper {
                plugin: plugin.name().as_str().to_owned(),
            });
        };
        let pin = pin.to_string();
        match job {
            GateJob::Static => self.run_static(plugin, pin),
            GateJob::Build(BuildJob::Lint) => self.run_lint(plugin, pin, steps),
            GateJob::Build(BuildJob::Test) => self.run_test(plugin, pin, steps),
        }
    }

    /// [`GateError::RatchetDegraded`] for a degraded ratchet, [`GateError::NotRun`] otherwise.
    fn gate_error(error: &Self::Error) -> GateError {
        match error {
            TemperError::RatchetDegraded { .. } => GateError::RatchetDegraded,
            _ => GateError::NotRun,
        }
    }
}

impl<S: CaptureSupervisor, R: RevisionFiles> TemperAdapter<S, R> {
    /// The Z2s checks, steps 1-4.
    fn run_static(&mut self, plugin: &Plugin, pin: String) -> Result<GateRun, ErrorOf<S, R>> {
        let allowlist = self.allowlist()?;
        let mut failing = self.raised()?;
        failing.extend(self.gate_config(&allowlist)?);
        let head = self.head_tree()?;
        let analysis = head.analyse();
        let scanned = self.scan(plugin.timeout())?;
        failing.extend(findings::blind(&analysis.blind_with(&scanned.allows)));
        failing.extend(findings::exceptions(
            scanned.requests,
            &analysis,
            &allowlist,
        ));
        let head_view = Tree::new(head.files());
        let base = self.base_files(&head_view)?;
        let base_view = Tree::new(
            base.files()
                .map(|(entry, bytes)| (entry, bytes.or_else(|| head_view.same_bytes(entry)))),
        );
        failing.extend(static_findings::inventory(
            &statics::test_inventory(&base_view, &head_view),
            &allowlist,
        ));
        let manifest_checks = manifests::check(&base_view, &head_view);
        // Never admitted: a blind spot is not waivable.
        let blind_targets: Vec<_> = manifest_checks.blind_targets.iter().collect();
        failing.extend(findings::blind(&blind_targets));
        failing.extend(static_findings::manifests(&manifest_checks, &allowlist));
        let drift = self.vendored_drift(&head, &pin)?;
        let conclusion = if failing.is_empty() {
            GateConclusion::Pass
        } else {
            GateConclusion::Fail
        };
        failing.extend(drift);
        self.document(plugin, pin, (conclusion, failing))
    }

    /// The run's `assay.findings/1` document: `plugin` at `pin` concludes `conclusion` with
    /// `findings`, as a clean exit.
    fn document(
        &self,
        plugin: &Plugin,
        pin: String,
        (conclusion, findings): (GateConclusion, Vec<Finding>),
    ) -> Result<GateRun, ErrorOf<S, R>> {
        self.settings
            .steps
            .document(plugin, pin, (conclusion, findings))
    }

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
        HeadTree::new(entries, blobs).map_err(|count| TemperError::Malformed {
            revision: Revision::Head,
            path: TREE,
            detail: format!(
                "the reader returned {} blobs for {} files",
                count.read, count.asked
            ),
        })
    }

    /// Step 4's base tree: every entry, with the bytes of its manifests and of the Rust files
    /// `head` does not hold unchanged, in one bounded read.
    fn base_files(&self, head: &Tree<'_>) -> Result<Files, ErrorOf<S, R>> {
        let failed = |source| TemperError::Revision {
            revision: Revision::Base,
            path: TREE,
            source,
        };
        let entries = self.revisions.list(Revision::Base, None).map_err(failed)?;
        let wanted = |entry: &TreeEntry| statics::base_needs_bytes(entry, head);
        let selected: Vec<&TreeEntry> = entries.iter().filter(|entry| wanted(entry)).collect();
        let blobs = self.revisions.read(&selected).map_err(failed)?;
        Files::new(entries, blobs, wanted).map_err(|count| TemperError::Malformed {
            revision: Revision::Base,
            path: TREE,
            detail: format!(
                "the reader returned {} blobs for {} files",
                count.read, count.asked
            ),
        })
    }

    /// Step 2: the exception requests and `allow` hits of the pinned scan over the head tree.
    fn scan(&mut self, timeout: Duration) -> Result<scan::Scanned, ErrorOf<S, R>> {
        let spec = ProcessSpec {
            program: &self.settings.node,
            args: &self.settings.scan_args,
            cwd: &self.settings.steps.head_tree,
            env: &self.settings.steps.env,
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
        scan::scanned(&run.stdout).map_err(|detail| TemperError::ScanOutput { detail })
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
