//! The `builtin:weigh-in` adapter in experimental mode (`docs/architecture/04-pipeline.md` §9,
//! 03 §2.2, #109). Until Weigh-In ships `weighin ci-check` (D-WI, #44) it reads the head's
//! journals and decision atoms in their documented formats (`docs/decisions/0003`), never links
//! or runs Weigh-In (P8), and reports 04 §9 rule 1's two sets as warnings: open, triaged items
//! that block (`weigh-in-open-blocking`) and open items not triaged yet
//! (`weigh-in-pending-triage`). It runs non-required only and never fails the gate: G2 is
//! enforced by CP1 and by convention meanwhile.
//!
//! Journals are the regular `*.jsonl` files directly under `.kb/_ledger/journal/`; atoms the
//! regular `*.md` files directly under `.kb/_intake/decisions/`. Both come from the PR, so they
//! are read through [`RevisionFiles`] in one batch whose listed size is capped first (H-13). A
//! file that is not exactly the documented form is refused whole and reported
//! (`weigh-in-unreadable`), never read in part (wi-b5affa).

mod atom;
mod error;
mod findings;
mod id;
mod journal;
mod ledger;

use std::collections::HashMap;

use assay_app::{Adapter, GateDocument, GateJob, GateRun, GateRunner, Plugin, Termination};
use assay_domain::{Finding, FindingsDocument, FindingsSchema, GateConclusion, Source, SourceKind};

pub use error::{WeighInError, WeighInSettingsError};

use self::findings::Refused;
use self::id::ItemId;
use self::ledger::Journal;
use crate::revision::{EntryKind, Revision, RevisionFiles, TreeEntry};

/// The journal directory.
const JOURNALS: &str = ".kb/_ledger/journal";
/// The decision atom directory.
const DECISIONS: &str = ".kb/_intake/decisions";
/// The directory listed once for both (and how [`WeighInError::Revision`] names the listing and
/// the batch read, and where a ledger past the limits is reported).
const LEDGER: &str = ".kb";
/// `.kb` and every directory on the way to [`JOURNALS`] and [`DECISIONS`]. A tree entry at one
/// of them (a file, symlink or submodule) hides everything below it, so it is refused rather
/// than read as an empty ledger (wi-b5affa).
const DIRECTORIES: [&str; 5] = [LEDGER, ".kb/_ledger", ".kb/_intake", JOURNALS, DECISIONS];
/// Most journal and atom files read.
const MAX_LEDGER_FILES: usize = 10_000;
/// Most bytes of journals and atoms read, by listed size, before anything is read.
const MAX_LEDGER_BYTES: u64 = 16 << 20;

/// What a file under one of the two directories is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Journal,
    Atom,
}

/// The Weigh-In adapter, generic over the reader of the PR's trees.
#[derive(Debug)]
pub struct WeighInAdapter<R> {
    revisions: R,
    run_key: String,
}

impl<R> WeighInAdapter<R> {
    /// An adapter that reads the PR head through `revisions` (pass `&revisions` to keep it) and
    /// writes documents for run `run_key`.
    ///
    /// # Errors
    ///
    /// [`WeighInSettingsError::EmptyRunKey`] when `run_key` is empty.
    pub fn new(revisions: R, run_key: impl Into<String>) -> Result<Self, WeighInSettingsError> {
        let run_key = run_key.into();
        if run_key.is_empty() {
            return Err(WeighInSettingsError::EmptyRunKey);
        }
        Ok(Self { revisions, run_key })
    }
}

impl<R: RevisionFiles> GateRunner for WeighInAdapter<R> {
    type Error = WeighInError<R::Error>;

    /// Reads the head's journals and atoms and reports the blocking and pending-triage sets,
    /// and every file it refused, as one `assay.findings/1` document that concludes `pass` when
    /// it reports nothing and `neutral` otherwise; never `fail`.
    ///
    /// # Errors
    ///
    /// [`WeighInError`] without reading anything when `plugin` is not weigh-in, is required, or
    /// `job` is not `gates-static`; when the head cannot be listed or read, or the reader
    /// returns another number of blobs than asked for.
    fn run(&mut self, plugin: &Plugin, job: GateJob) -> Result<GateRun, Self::Error> {
        let Adapter::WeighIn { pin } = plugin.adapter() else {
            return Err(WeighInError::NotWeighIn {
                plugin: plugin.name().as_str().to_owned(),
            });
        };
        if plugin.required() {
            return Err(WeighInError::Required {
                plugin: plugin.name().as_str().to_owned(),
            });
        }
        if job != GateJob::Static {
            return Err(WeighInError::OutsideStatic { job });
        }
        let findings = self.warnings()?;
        let conclusion = if findings.is_empty() {
            GateConclusion::Pass
        } else {
            GateConclusion::Neutral
        };
        let document = FindingsDocument {
            schema: FindingsSchema::V1,
            source: Source {
                kind: SourceKind::Gate,
                name: plugin.name().as_str().to_owned(),
                version: pin.to_string(),
                role: None,
                // The adapter keeps the key for the next run; the document owns its copy.
                run_key: self.run_key.clone(),
            },
            conclusion: Some(conclusion),
            findings,
            // Weigh-In does not attest step results (#153): `decide` reads its gate as unknown.
            failed_steps: None,
            stats: None,
        };
        let bytes = serde_json::to_vec(&document).map_err(WeighInError::Document)?;
        Ok(GateRun {
            termination: Termination::Exited(0),
            document: GateDocument::Written(bytes),
        })
    }
}

impl<R: RevisionFiles> WeighInAdapter<R> {
    /// The head's journals and atoms, read and folded into the document's findings.
    fn warnings(&self) -> Result<Vec<Finding>, WeighInError<R::Error>> {
        let entries = self.list()?;
        let mut refused: Vec<Refused<'_>> = entries
            .iter()
            .filter(|entry| DIRECTORIES.contains(&entry.path.as_str()))
            .map(|entry| Refused {
                path: &entry.path,
                line: None,
                why: "is not a directory (a file, symlink or submodule), so nothing below it \
                      was read",
            })
            .collect();
        let mut wanted = select(JOURNALS, &entries, Kind::Journal, &mut refused);
        wanted.extend(select(DECISIONS, &entries, Kind::Atom, &mut refused));
        if !within_limits(&wanted) {
            refused.push(Refused {
                path: LEDGER,
                line: None,
                why: "holds more journal and atom files or bytes than this adapter reads \
                      (10,000 files, 16 MiB), so none was read",
            });
            return Ok(findings::warnings(&refused, &[], &HashMap::new()));
        }
        let blobs = self.read(&wanted)?;
        let (journals, atoms) = parse(&wanted, &blobs, &mut refused);
        let mut named: HashMap<&ItemId, &str> = HashMap::new();
        for (id, path) in &atoms {
            named.entry(id).or_insert(path);
        }
        Ok(findings::warnings(
            &refused,
            &ledger::items(&journals),
            &named,
        ))
    }

    /// The bytes of each of `wanted`, in one read.
    fn read(&self, wanted: &[(&TreeEntry, Kind)]) -> Result<Vec<Vec<u8>>, WeighInError<R::Error>> {
        let entries: Vec<&TreeEntry> = wanted.iter().map(|(entry, _)| *entry).collect();
        let blobs = self
            .revisions
            .read(&entries)
            .map_err(|source| WeighInError::Revision {
                path: LEDGER,
                source,
            })?;
        if blobs.len() == entries.len() {
            Ok(blobs)
        } else {
            Err(WeighInError::BlobCount {
                asked: entries.len(),
                read: blobs.len(),
            })
        }
    }

    /// The head's entries at or under `.kb`, in one listing: an entry at `.kb` itself or at any
    /// directory on the way to the journals or atoms is one that is not a directory.
    fn list(&self) -> Result<Vec<TreeEntry>, WeighInError<R::Error>> {
        self.revisions
            .list(Revision::Head, Some(LEDGER))
            .map_err(|source| WeighInError::Revision {
                path: LEDGER,
                source,
            })
    }
}

/// The regular files directly under `dir` in `entries` whose name ends in the suffix of
/// `kind`, tagged with it. Such a name that is a symlink or a submodule goes to `refused`;
/// anything else is not Weigh-In's and is skipped.
fn select<'a>(
    dir: &str,
    entries: &'a [TreeEntry],
    kind: Kind,
    refused: &mut Vec<Refused<'a>>,
) -> Vec<(&'a TreeEntry, Kind)> {
    let suffix = match kind {
        Kind::Journal => ".jsonl",
        Kind::Atom => ".md",
    };
    let mut wanted = Vec::new();
    for entry in entries {
        let path = entry.path.as_str();
        let Some(name) = path
            .strip_prefix(dir)
            .and_then(|rest| rest.strip_prefix('/'))
        else {
            continue;
        };
        if name.contains('/') || !name.ends_with(suffix) {
            continue;
        }
        match entry.kind {
            EntryKind::File => wanted.push((entry, kind)),
            EntryKind::Symlink | EntryKind::Submodule => refused.push(Refused {
                path,
                line: None,
                why: "is a symlink or a submodule, not a regular file",
            }),
        }
    }
    wanted
}

/// The journals and Weigh-In atoms among `wanted`, whose bytes are `blobs`; each file that is
/// not exactly its documented form goes to `refused` instead.
fn parse<'a>(
    wanted: &[(&'a TreeEntry, Kind)],
    blobs: &[Vec<u8>],
    refused: &mut Vec<Refused<'a>>,
) -> (Vec<Journal<'a>>, Vec<(ItemId, &'a str)>) {
    let mut journals = Vec::new();
    let mut atoms = Vec::new();
    for ((entry, kind), bytes) in wanted.iter().zip(blobs) {
        let path = entry.path.as_str();
        match kind {
            Kind::Journal => match journal::parse(bytes) {
                Ok(events) => journals.push(Journal { path, events }),
                Err(refusal) => refused.push(Refused {
                    path,
                    line: refusal.line,
                    why: refusal.fault.text(),
                }),
            },
            Kind::Atom => match atom::item(file_name(path), bytes) {
                Ok(Some(id)) => atoms.push((id, path)),
                Ok(None) => {}
                Err(fault) => refused.push(Refused {
                    path,
                    line: None,
                    why: fault.text(),
                }),
            },
        }
    }
    (journals, atoms)
}

/// Whether `wanted` is within [`MAX_LEDGER_FILES`] and [`MAX_LEDGER_BYTES`] by listed size; a
/// file with no listed size is past them.
fn within_limits(wanted: &[(&TreeEntry, Kind)]) -> bool {
    wanted.len() <= MAX_LEDGER_FILES
        && wanted
            .iter()
            .try_fold(0_u64, |total, (entry, _)| {
                entry
                    .size
                    .and_then(|size| total.checked_add(size))
                    .filter(|total| *total <= MAX_LEDGER_BYTES)
            })
            .is_some()
}

/// The last segment of `path`.
fn file_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}
