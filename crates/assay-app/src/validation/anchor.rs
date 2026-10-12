//! Step 2 of 04 §4: locates a finding's evidence in the tree, relocates its lines, replaces its
//! quotes with the tree's own lines and computes its fingerprint.

use std::num::NonZeroU64;

use assay_domain::{Caller, Disposition, Finding, FindingKind, Fingerprint, IntroducedBy, Side};

use super::change::{Change, LineSpan};
use super::evidence::{self, Match};
use super::index::{Files, Indexed};
use super::symbol;

/// What step 2 established about a finding.
pub(super) struct Anchored {
    /// Its file's path at head, rename-resolved.
    pub(super) file_at_head: String,
    /// The function its line belongs to, empty at file scope.
    pub(super) enclosing_symbol: String,
    /// How many earlier matches of its evidence the same function holds.
    pub(super) occurrence_index: u32,
    /// Its fingerprint, over the tree's lines that replaced its evidence.
    pub(super) fingerprint: Fingerprint,
    /// Whether its line is in a hunk.
    pub(super) in_diff: bool,
    /// Whether it can be an inline comment: its line, and `end_line` if any, are in one hunk.
    pub(super) inline: bool,
}

/// Step 2 for `finding`: finds its evidence in the cited file (base for `LEFT` and deleted
/// code), moves `line` to the nearest match and `end_line` with it, checks an omission anchors
/// on a signature and an interaction's caller evidence exists, and replaces `evidence` and
/// `caller.evidence` with the lines they matched.
///
/// An `end_line` outside the file or outside the hunk holding `line` is dropped, and the
/// finding is then not inline. Evidence with too little content is `evidence-missing`.
pub(super) fn anchor<'c>(
    change: &'c Change,
    files: &mut Files<'c>,
    finding: &mut Finding,
) -> Result<Anchored, Disposition> {
    if !evidence::has_enough_content(&finding.evidence) {
        return Err(Disposition::EvidenceMissing);
    }
    let located = change.locate(&finding.file, finding.side.unwrap_or(Side::Right));
    let text = located.text.ok_or(Disposition::EvidenceMissing)?;
    let indexed = files.index(text);
    let matches = indexed.find(&finding.evidence);
    let found =
        evidence::nearest(&matches, finding.line.get()).ok_or(Disposition::EvidenceMissing)?;
    let on_signature = indexed
        .line(found.line)
        .and_then(symbol::signature_name)
        .is_some();
    if finding.kind == FindingKind::Omission && !on_signature {
        return Err(Disposition::EvidenceMissing);
    }
    let enclosing_symbol = indexed
        .function_at(found.line)
        .ok_or(Disposition::EvidenceMissing)?;
    let hunk = located.hunk_of(found.line);
    let end_line = shifted(finding.end_line, finding.line.get(), found.line);
    let end_line_holds = end_line.is_none_or(|end| within(indexed, hunk, end.get()));
    finding.end_line = end_line.filter(|_| end_line_holds);
    finding.line = NonZeroU64::new(found.line).ok_or(Disposition::EvidenceMissing)?;
    finding.side = Some(located.side);
    finding.evidence = indexed.span(found).ok_or(Disposition::EvidenceMissing)?;
    let occurrence_index = occurrence(indexed, &matches, found, enclosing_symbol);
    let fingerprint = finding
        .anchor(&located.file_at_head, enclosing_symbol)
        .fingerprint(occurrence_index)
        .ok_or(Disposition::EvidenceMissing)?;
    let enclosing_symbol = enclosing_symbol.to_owned();
    if finding.introduced_by == Some(IntroducedBy::Interaction) {
        let caller = finding.caller.as_mut().ok_or(Disposition::Schema)?;
        relocate_caller(change, files, caller)?;
    }
    Ok(Anchored {
        in_diff: hunk.is_some(),
        inline: hunk.is_some() && end_line_holds,
        file_at_head: located.file_at_head,
        enclosing_symbol,
        occurrence_index,
        fingerprint,
    })
}

/// Whether `line` is in the file and, if the finding's line is in `hunk`, in that hunk too.
fn within(indexed: &Indexed<'_>, hunk: Option<LineSpan>, line: u64) -> bool {
    line <= indexed.line_count() && hunk.is_none_or(|span| span.contains(line))
}

/// How many matches before `found` lie in the same function, `symbol`.
fn occurrence(indexed: &Indexed<'_>, matches: &[Match], found: Match, symbol: &str) -> u32 {
    let earlier = matches
        .iter()
        .filter(|other| other.line < found.line && indexed.function_at(other.line) == Some(symbol))
        .count();
    // One match per line at most, and no file read here has 4 billion lines.
    u32::try_from(earlier).unwrap_or(u32::MAX)
}

/// Relocates an interaction's `caller` to its evidence at head, which must exist, and replaces
/// that evidence with the lines it matched.
fn relocate_caller<'c>(
    change: &'c Change,
    files: &mut Files<'c>,
    caller: &mut Caller,
) -> Result<(), Disposition> {
    if !evidence::has_enough_content(&caller.evidence) {
        return Err(Disposition::EvidenceMissing);
    }
    let text = change
        .locate(&caller.file, Side::Right)
        .text
        .ok_or(Disposition::EvidenceMissing)?;
    let indexed = files.index(text);
    let found = evidence::nearest(&indexed.find(&caller.evidence), caller.line.get())
        .ok_or(Disposition::EvidenceMissing)?;
    caller.line = NonZeroU64::new(found.line).ok_or(Disposition::EvidenceMissing)?;
    caller.evidence = indexed.span(found).ok_or(Disposition::EvidenceMissing)?;
    Ok(())
}

/// `end_line` moved with `line` from `from` to `to`, keeping the span; dropped if it preceded
/// `from`.
fn shifted(end_line: Option<NonZeroU64>, from: u64, to: u64) -> Option<NonZeroU64> {
    let span = end_line?.get().checked_sub(from)?;
    NonZeroU64::new(to.checked_add(span)?)
}
