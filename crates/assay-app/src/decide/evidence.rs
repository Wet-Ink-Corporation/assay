//! Gate findings with Z4's own evidence and fingerprint (03 §3: no producer-supplied `fp`;
//! 03 §2.1 step 6: a gate's evidence is the tree's line at its `file:line`).
//!
//! A finding is *verified* when its file and line exist on its side and that line, normalised,
//! is the first line of content of its evidence. Its evidence is then replaced by the tree's line
//! and its fingerprint is taken over it, in its enclosing function, with its occurrence index.
//! Any other finding keeps its evidence and is fingerprinted at file scope, and its rule is
//! dropped, so no waiver can clear it (fail closed): a gate's synthesised spot, a deleted file,
//! a file past the read budget, or a quote the tree does not hold.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use assay_domain::{FileChange, Finding, Fingerprint, Side, normalise};

use super::DecideError;
use crate::config::CommitId;
use crate::plan::{EntryKind, PullRequestObjects, TreeEntry};
use crate::validation::enclosing_functions;

/// The most distinct files read for gate evidence in one run (H-13).
pub(super) const MAX_EVIDENCE_FILES: usize = 1_000;

/// The most bytes read for gate evidence in one run, charged by listed size before the read.
pub(super) const MAX_EVIDENCE_BYTES: u64 = 64 << 20;

/// The most characters evidence may hold (`findings-1.schema.json`).
const MAX_EVIDENCE_CHARS: usize = 400;

/// The texts gate findings cite, as far as the budget reads, with renames resolved.
#[derive(Debug, Default)]
pub(super) struct Texts {
    head: BTreeMap<String, String>,
    base: BTreeMap<String, String>,
    /// Head path by base path, for files the change renamed.
    renamed: BTreeMap<String, String>,
}

/// The paths gate findings cite, by side.
#[derive(Debug, Default)]
pub(super) struct Cited<'f> {
    head: BTreeSet<&'f str>,
    base: BTreeSet<&'f str>,
}

impl<'f> Cited<'f> {
    /// Adds the file `finding` cites on its side (head unless it says base).
    pub(super) fn add(&mut self, finding: &'f Finding) {
        let paths = match finding.side.unwrap_or(Side::Right) {
            Side::Right => &mut self.head,
            Side::Left => &mut self.base,
        };
        paths.insert(finding.file.as_str());
    }
}

/// Reads the files `cited` names on each side, head first and in path order, until the budget
/// is spent.
pub(super) fn read_texts<O: PullRequestObjects, S, A>(
    objects: &O,
    (base, head): (&CommitId, &CommitId),
    files: &[FileChange],
    cited: &Cited<'_>,
) -> Result<Texts, DecideError<O::Error, S, A>> {
    let mut budget = Budget {
        files: MAX_EVIDENCE_FILES,
        bytes: MAX_EVIDENCE_BYTES,
    };
    let renamed = files
        .iter()
        .filter_map(|file| {
            let from = file.renamed_from.as_ref()?;
            // The map owns both paths; the change keeps its own.
            Some((from.clone(), file.path.clone()))
        })
        .collect();
    let head = side_texts(objects, head, cited.head.iter().copied(), &mut budget)?;
    let base = side_texts(objects, base, cited.base.iter().copied(), &mut budget)?;
    Ok(Texts {
        head,
        base,
        renamed,
    })
}

/// What the evidence reads may still spend.
struct Budget {
    files: usize,
    bytes: u64,
}

/// The UTF-8 regular files among `paths` at `commit` that fit the budget, read in one batch.
fn side_texts<'p, O: PullRequestObjects, S, A>(
    objects: &O,
    commit: &CommitId,
    paths: impl Iterator<Item = &'p str>,
    budget: &mut Budget,
) -> Result<BTreeMap<String, String>, DecideError<O::Error, S, A>> {
    let mut paths = paths.peekable();
    if paths.peek().is_none() {
        return Ok(BTreeMap::new());
    }
    let listed = objects.list(commit, None).map_err(DecideError::Objects)?;
    let by_path: BTreeMap<&str, &TreeEntry> = listed
        .iter()
        .filter(|entry| entry.kind == EntryKind::File)
        .map(|entry| (entry.path.as_str(), entry))
        .collect();
    let mut wanted: Vec<&TreeEntry> = Vec::new();
    for path in paths {
        let Some(entry) = by_path.get(path) else {
            continue;
        };
        let size = entry.size.unwrap_or(u64::MAX);
        if budget.files == 0 || size > budget.bytes {
            break;
        }
        budget.files = budget.files.saturating_sub(1);
        budget.bytes = budget.bytes.saturating_sub(size);
        wanted.push(entry);
    }
    if wanted.is_empty() {
        return Ok(BTreeMap::new());
    }
    let blobs = objects.read(&wanted).map_err(DecideError::Objects)?;
    if blobs.len() != wanted.len() {
        return Err(DecideError::ShortRead {
            asked: wanted.len(),
            returned: blobs.len(),
        });
    }
    Ok(wanted
        .into_iter()
        .zip(blobs)
        // The listing is dropped here, so the map owns its keys; a file that is not UTF-8 is not
        // read, and its findings stay unverified.
        .filter_map(|(entry, blob)| Some((entry.path.clone(), String::from_utf8(blob).ok()?)))
        .collect())
}

/// One cited file, indexed once: its lines, the function each belongs to, and how many earlier
/// lines of that function normalise alike.
struct Indexed<'t> {
    lines: Vec<&'t str>,
    functions: Vec<&'t str>,
    occurrences: Vec<u32>,
}

impl<'t> Indexed<'t> {
    fn of(text: &'t str) -> Self {
        let lines: Vec<&str> = text.lines().collect();
        let functions = enclosing_functions(text);
        let mut seen: HashMap<(&str, String), u32> = HashMap::new();
        let occurrences = lines
            .iter()
            .zip(&functions)
            .map(|(line, function)| {
                let count = seen.entry((*function, normalise(line))).or_insert(0);
                let earlier = *count;
                *count = count.saturating_add(1);
                earlier
            })
            .collect();
        Self {
            lines,
            functions,
            occurrences,
        }
    }
}

/// Anchors gate findings against the texts, indexing each file once.
pub(super) struct Anchors<'t> {
    texts: &'t Texts,
    indexed: HashMap<(Side, &'t str), Indexed<'t>>,
}

impl<'t> Anchors<'t> {
    pub(super) fn new(texts: &'t Texts) -> Self {
        Self {
            texts,
            indexed: HashMap::new(),
        }
    }

    /// `finding`'s fingerprint, with its evidence replaced and its rule kept when it is verified
    /// and its rule dropped otherwise; `None` when not even its own evidence has content.
    pub(super) fn anchor(&mut self, finding: &mut Finding) -> Option<Fingerprint> {
        let side = finding.side.unwrap_or(Side::Right);
        let (function, occurrence) =
            if let Some((line, function, occurrence)) = self.verified(side, finding) {
                finding.evidence = line.chars().take(MAX_EVIDENCE_CHARS).collect();
                finding.side = Some(side);
                (function, occurrence)
            } else {
                finding.rule = None;
                ("", 0)
            };
        let texts: &'t Texts = self.texts;
        let file_at_head = match side {
            Side::Right => finding.file.as_str(),
            Side::Left => texts
                .renamed
                .get(&finding.file)
                .map_or(finding.file.as_str(), String::as_str),
        };
        finding
            .anchor(file_at_head, function)
            .fingerprint(occurrence)
    }

    /// The cited line, its function and occurrence index, when the finding is verified.
    fn verified(&mut self, side: Side, finding: &Finding) -> Option<(&'t str, &'t str, u32)> {
        let all: &'t Texts = self.texts;
        let texts = match side {
            Side::Right => &all.head,
            Side::Left => &all.base,
        };
        let (path, text) = texts.get_key_value(&finding.file)?;
        let indexed = self
            .indexed
            .entry((side, path.as_str()))
            .or_insert_with(|| Indexed::of(text));
        let index = usize::try_from(finding.line.get().checked_sub(1)?).ok()?;
        let line = *indexed.lines.get(index)?;
        let quoted = finding.evidence.lines().find_map(content)?;
        if content(line)? != quoted {
            return None;
        }
        Some((
            line,
            *indexed.functions.get(index)?,
            *indexed.occurrences.get(index)?,
        ))
    }
}

/// `line` normalised, when it holds something other than blanks and wildcards.
fn content(line: &str) -> Option<String> {
    let normalised = normalise(line);
    normalised
        .chars()
        .any(|c| c != '…' && !c.is_whitespace())
        .then_some(normalised)
}
