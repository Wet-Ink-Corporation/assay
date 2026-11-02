//! The temper adapter in `gates-lint`, zone Z2b (`docs/architecture/03-contracts.md` §2.1 steps
//! 5-7): first the plugin's declared `lint` steps (`fmt`, `deny`) under their own timeouts,
//! before any build script or proc-macro has run in the tree; then the base's pinned `temper
//! baseline --check --locked --base <base_sha> --findings-out target/temper/findings.json` from
//! the head checkout, its ratchet believed only against the merge base Z2s computed.
//!
//! A declared step a signal ended is run once more, and that run decides (06 F9, as in
//! `gates-test`); `baseline --check` is never retried. A run that does not exit (timeout,
//! signal after a step's one retry, output cap), or a `baseline --check` exit other than 0 or
//! 1, is handed to the gate use case as that termination, so it is the matching `gate-error`. A
//! step that exits non-zero fails the gate.

use std::fs;
use std::io;
use std::path::Path;

use assay_app::{BuildJob, GateDocument, GateRun, Plugin, Step, Termination};
use assay_domain::{Finding, GateConclusion};

use super::evidence::{self, MAX_EVIDENCE_BYTES, Unread};
use super::exceptions::Allowlist;
use super::gate_config::{self, TemperConfig};
use super::lint_findings::{self, Shown};
use super::ratchet::{self, Checked, Refused, Tool};
use super::steps::{Verdict, capture, stopped};
use super::{ErrorOf, TREE, TemperAdapter, TemperError};
use crate::capture::CaptureSupervisor;
use crate::exec::read_document;
use crate::revision::{Revision, RevisionFiles};

/// Where `baseline --check` writes its collection and ratchet, relative to the head tree; temper
/// accepts only paths under `target/` or `.temper/`.
pub(super) const FINDINGS_OUT: &str = "target/temper/findings.json";

/// Largest findings document read, in bytes; a larger one is refused unparsed (H-13).
pub(super) const MAX_FINDINGS_OUT_BYTES: u64 = 64 << 20;

/// How `baseline --check` is named in errors.
const BASELINE_CHECK: &str = "temper baseline --check";

/// How a failed `baseline --check` is named among the document's `failed_steps` (#153).
const BASELINE_STEP: &str = "baseline";

impl<S: CaptureSupervisor, R: RevisionFiles> TemperAdapter<S, R> {
    /// Runs `gates-lint` for `plugin`, whose declared steps are `steps`.
    pub(super) fn run_lint(
        &mut self,
        plugin: &Plugin,
        pin: String,
        steps: &[Step],
    ) -> Result<GateRun, ErrorOf<S, R>> {
        let Some(args) = self.settings.lint_args() else {
            return Err(TemperError::NoRatchetBase);
        };
        let allowlist = self.allowlist()?;
        let required = self.base_sources()?;
        // The steps read the tree before `baseline --check` builds it (build scripts and
        // proc-macros run there).
        let mut failed_steps = Vec::new();
        for step in steps.iter().filter(|step| step.job() == BuildJob::Lint) {
            match self.step_retried(step)? {
                Termination::Exited(0) => {}
                Termination::Exited(_) => failed_steps.push(step.name().as_str().to_owned()),
                other => return Ok(stopped(other)),
            }
        }
        let out = self.settings.steps.head_tree.join(FINDINGS_OUT);
        remove_stale(&self.settings.steps.head_tree, Path::new(FINDINGS_OUT))?;
        let run = capture(
            &mut self.supervisor,
            &self.settings.steps,
            (BASELINE_CHECK, &self.settings.node, &args),
            plugin.timeout(),
        )?;
        let exit_failed = match run {
            Termination::Exited(0) => false,
            Termination::Exited(1) => true,
            other => return Ok(stopped(other)),
        };
        let checked = self.checked(&out, &required)?;
        if checked.ok == exit_failed {
            return Err(TemperError::FindingsOutput {
                detail: "temper's exit code disagrees with ratchet.ok".to_owned(),
            });
        }
        // temper's ratchet shows its causes as display findings only in part (capped, and
        // compile errors only when located), so its failure is a step of its own (#153). A
        // declared step of that name that failed already says so: the names stay distinct.
        if exit_failed && !failed_steps.iter().any(|name| name == BASELINE_STEP) {
            failed_steps.push(BASELINE_STEP.to_owned());
        }
        let findings = self.lint_findings(&checked, &allowlist)?;
        self.document(plugin, pin, findings.verdict(failed_steps))
    }

    /// The findings document `baseline --check` left at `out`, read strictly and believed only
    /// against the expected merge base and with every source in `required`.
    fn checked(&self, out: &Path, required: &[Tool]) -> Result<Checked, ErrorOf<S, R>> {
        let bytes = findings_out(out, MAX_FINDINGS_OUT_BYTES)?;
        let merge_base = self
            .settings
            .ratchet
            .as_ref()
            .map(|ratchet| ratchet.merge_base.as_str())
            .ok_or(TemperError::NoRatchetBase)?;
        ratchet::check(&bytes, merge_base, required).map_err(|refused| match refused {
            Refused::Malformed(detail) => TemperError::FindingsOutput { detail },
            Refused::Degraded(why) => TemperError::RatchetDegraded { why },
        })
    }

    /// The sources base `.claude/temper.json` enables, which the ratchet must have collected
    /// whatever head's config says (#210 F3); temper's defaults when base has none. Read before
    /// anything runs in the tree.
    fn base_sources(&self) -> Result<Vec<Tool>, ErrorOf<S, R>> {
        let Some(bytes) = self.read(Revision::Base, gate_config::PATH)? else {
            return Ok(TemperConfig::default().sources());
        };
        TemperConfig::parse(&bytes)
            .map(|config| config.sources())
            .map_err(|detail| TemperError::Malformed {
                revision: Revision::Base,
                path: gate_config::PATH,
                detail,
            })
    }

    /// The failures and the display findings of `checked`, with evidence from the trees.
    fn lint_findings(
        &self,
        checked: &Checked,
        allowlist: &Allowlist,
    ) -> Result<LintFindings, ErrorOf<S, R>> {
        let failures = lint_findings::failures(checked, allowlist);
        let mut findings = failures.findings;
        let shown = lint_findings::shown(checked, findings.len());
        let lines = evidence::gather(
            &self.revisions,
            shown.iter().filter_map(Shown::cites),
            MAX_EVIDENCE_BYTES,
        )
        .map_err(|unread| match unread {
            Unread::Reader { revision, source } => TemperError::Revision {
                revision,
                path: TREE,
                source,
            },
            Unread::Count { revision, count } => TemperError::Malformed {
                revision,
                path: TREE,
                detail: format!(
                    "the reader returned {} blobs for {} files",
                    count.read, count.asked
                ),
            },
        })?;
        findings.extend(
            shown
                .into_iter()
                .map(|shown| lint_findings::display(shown, &lines)),
        );
        findings.truncate(lint_findings::MAX_REPORTED);
        Ok(LintFindings {
            failing: failures.failing,
            all_reported: failures.all_reported,
            findings,
        })
    }
}

/// The findings of one `gates-lint` run.
struct LintFindings {
    /// Whether a `raised` or unadmitted `exception` entry was found.
    failing: bool,
    /// Whether each such entry is one of `findings`.
    all_reported: bool,
    findings: Vec<Finding>,
}

impl LintFindings {
    /// The run's verdict, after `failed_steps` failed without a finding: it fails on either.
    /// Failures past the report cap have no finding a waiver could name, so the run attests no
    /// step list then (#153).
    fn verdict(self, failed_steps: Vec<String>) -> Verdict {
        let conclusion = if self.failing || !failed_steps.is_empty() {
            GateConclusion::Fail
        } else {
            GateConclusion::Pass
        };
        Verdict {
            conclusion,
            findings: self.findings,
            failed_steps: self.all_reported.then_some(failed_steps),
        }
    }
}

/// Removes a findings document an earlier run or a restored `target/` left, so only this run's
/// can be read. Nothing there is fine; anything that cannot be removed is an error, and so is a
/// symlink at any directory between `head_tree` and the document (`target`, `target/temper`),
/// which would point the removal, and temper's write, out of the tree. `relative` is the
/// document's path below `head_tree`.
fn remove_stale<S, R>(head_tree: &Path, relative: &Path) -> Result<(), TemperError<S, R>> {
    let refused = |path: &Path, source| TemperError::FindingsOut {
        path: path.to_path_buf(),
        source,
    };
    let out = head_tree.join(relative);
    let mut directory = head_tree.to_path_buf();
    for component in relative.parent().into_iter().flat_map(Path::components) {
        directory.push(component);
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let symlink = io::Error::new(io::ErrorKind::InvalidInput, "a symlink");
                return Err(refused(&directory, symlink));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(source) => return Err(refused(&directory, source)),
        }
    }
    match fs::remove_file(&out) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(refused(&out, source)),
    }
}

/// The bytes of the regular file at `out`, never through a symlink and at most `cap` of them.
fn findings_out<S, R>(out: &Path, cap: u64) -> Result<Vec<u8>, TemperError<S, R>> {
    let document = read_document(out, cap).map_err(|source| TemperError::FindingsOut {
        path: out.to_path_buf(),
        source,
    })?;
    let detail = match document {
        GateDocument::Written(bytes) => return Ok(bytes),
        GateDocument::Missing => "temper wrote no findings document".to_owned(),
        GateDocument::NotAFile => "the findings document is not a regular file".to_owned(),
        GateDocument::TooLarge => format!("the findings document is over {cap} bytes"),
    };
    Err(TemperError::FindingsOutput { detail })
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;

    type Read = Result<Vec<u8>, TemperError<io::Error, io::Error>>;

    #[test]
    fn reads_a_document_of_exactly_the_cap() -> Result<(), Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        let out = dir.path().join("findings.json");
        fs::write(&out, b"0123456789")?;

        let read: Read = findings_out(&out, 10);

        assert_eq!(read.ok(), Some(b"0123456789".to_vec()));
        Ok(())
    }

    #[test]
    fn refuses_a_document_one_byte_past_the_cap() -> Result<(), Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        let out = dir.path().join("findings.json");
        fs::write(&out, b"0123456789A")?;

        let read: Read = findings_out(&out, 10);

        assert!(
            matches!(&read, Err(TemperError::FindingsOutput { detail }) if detail.contains("over 10 bytes")),
            "{read:?}"
        );
        Ok(())
    }

    #[test]
    fn removing_a_directory_in_the_documents_place_fails() -> Result<(), Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        fs::create_dir_all(dir.path().join(FINDINGS_OUT))?;

        let removed: Result<(), TemperError<io::Error, io::Error>> =
            remove_stale(dir.path(), Path::new(FINDINGS_OUT));

        assert!(
            matches!(removed, Err(TemperError::FindingsOut { .. })),
            "{removed:?}"
        );
        Ok(())
    }

    #[test]
    fn a_symlinked_directory_on_the_way_to_the_document_is_refused() -> Result<(), Box<dyn Error>> {
        for linked in ["target", "target/temper"] {
            let dir = tempfile::tempdir()?;
            let elsewhere = dir.path().join("elsewhere");
            fs::create_dir(&elsewhere)?;
            fs::write(elsewhere.join("findings.json"), b"{}")?;
            let head = dir.path().join("head");
            let link = head.join(linked);
            if let Some(parent) = link.parent() {
                fs::create_dir_all(parent)?;
            }
            std::os::unix::fs::symlink(&elsewhere, &link)?;

            let removed: Result<(), TemperError<io::Error, io::Error>> =
                remove_stale(&head, Path::new(FINDINGS_OUT));

            assert!(
                matches!(&removed, Err(TemperError::FindingsOut { path, .. }) if *path == link),
                "{linked}: {removed:?}"
            );
            assert!(
                elsewhere.join("findings.json").exists(),
                "{linked}: nothing outside the tree is removed"
            );
        }
        Ok(())
    }

    #[test]
    fn nothing_on_the_way_to_the_document_is_fine() -> Result<(), Box<dyn Error>> {
        let dir = tempfile::tempdir()?;

        let removed: Result<(), TemperError<io::Error, io::Error>> =
            remove_stale(dir.path(), Path::new(FINDINGS_OUT));

        assert!(removed.is_ok(), "{removed:?}");
        Ok(())
    }
}
