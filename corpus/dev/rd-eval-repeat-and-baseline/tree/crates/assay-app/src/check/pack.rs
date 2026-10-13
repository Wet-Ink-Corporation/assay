//! The local review's context pack: the changed files with their status, then the diff, each
//! capped (04 §3 names both; local mode has no base `REVIEW.md` or admit pack yet, #85, #72).
//!
//! A file's line or patch goes in whole or not at all, and the pack says how many it left out,
//! so the model never reads a diff cut mid-hunk.

use crate::validation::{DiffPaths, FileDiff};

/// The most bytes of file list lines (`M <path>\n`) in a context pack: 32 KiB.
pub const MAX_FILE_LIST_BYTES: usize = 32 << 10;

/// The most bytes of diff in a context pack: 120 KiB (04 §3).
pub const MAX_DIFF_BYTES: usize = 120 << 10;

/// The pack for `diffs` (the change's files, in order) and their patch texts, one per diff
/// (`None` for one the patches port withheld, which counts as not shown). Linear in the listed
/// lines and patches; each section stops growing at its cap.
pub(super) fn context_pack(diffs: &[FileDiff], patches: &[Option<String>]) -> String {
    let mut pack = format!("Changed files ({}):\n", diffs.len());
    let lines = diffs.iter().map(|diff| status_line(&diff.paths));
    let unlisted = append_capped(&mut pack, lines, MAX_FILE_LIST_BYTES);
    if unlisted > 0 {
        let count = unlisted.to_string();
        pack.extend(["[", &count, " more ", files(unlisted), " not listed]\n"]);
    }
    pack.push_str("\nDiff:\n");
    let withheld = patches.iter().filter(|text| text.is_none()).count();
    let unshown =
        append_capped(&mut pack, patches.iter().flatten(), MAX_DIFF_BYTES).saturating_add(withheld);
    if unshown > 0 {
        let count = unshown.to_string();
        let cap = MAX_DIFF_BYTES.to_string();
        pack.extend([
            "[",
            &count,
            " more ",
            files(unshown),
            " not shown: the diff stops at ",
            &cap,
            " bytes]\n",
        ]);
    }
    pack
}

/// Appends each of `pieces` that still fits in `cap` bytes of appended text, and returns how
/// many non-empty pieces did not.
fn append_capped<S: AsRef<str>>(
    pack: &mut String,
    pieces: impl Iterator<Item = S>,
    cap: usize,
) -> usize {
    let mut used = 0_usize;
    let mut left_out = 0_usize;
    for piece in pieces {
        let piece = piece.as_ref();
        if piece.is_empty() {
            continue;
        }
        match used.checked_add(piece.len()).filter(|total| *total <= cap) {
            Some(total) => {
                used = total;
                pack.push_str(piece);
            }
            None => left_out = left_out.saturating_add(1),
        }
    }
    left_out
}

/// `file` or `files`.
const fn files(count: usize) -> &'static str {
    if count == 1 { "file" } else { "files" }
}

/// One file's line: its status letter and path, both paths for a rename.
fn status_line(paths: &DiffPaths) -> String {
    match paths {
        DiffPaths::Added { head } => format!("A {head}\n"),
        DiffPaths::Deleted { base } => format!("D {base}\n"),
        DiffPaths::Modified { path } => format!("M {path}\n"),
        DiffPaths::Renamed { base, head } => format!("R {base} -> {head}\n"),
    }
}
