//! What explains a long run of model text locally (03 §3, #87): the snapshot's files, which the
//! reviewer could read, and the run's own base and snapshot commits.
//!
//! The developer's interactive login is the CLI's to keep, so local mode holds no token and
//! scans as Z4 does, for unexplained runs only.

use crate::plan::{EntryKind, PullRequestObjects, TreeEntry};
use crate::validation::MAX_INDEXED_BYTES;

use super::CheckError;

/// Most bytes asked of the objects port in one read: a quarter of the forge's default cap
/// (`TreeLimits::max_total_bytes`, 32 MiB), so a read stays inside an adapter's own limit.
const READ_BATCH_BYTES: u64 = 8 << 20;

/// The texts of the snapshot's files and symlinks among `head`, in path order, read in batches
/// of at most [`READ_BATCH_BYTES`].
///
/// A file larger than one batch, and any file that would take the total past
/// [`MAX_INDEXED_BYTES`], is left out. Leaving a text out only makes more runs unexplained, so
/// the scan drops more findings, never fewer (fail closed, wi-b5affa).
///
/// Cost: one read per batch; the bytes held are at most [`MAX_INDEXED_BYTES`].
///
/// # Errors
///
/// [`CheckError::Objects`] when a batch cannot be read, and [`CheckError::ShortRead`] when the
/// port returns another number of blobs than asked for.
pub(super) fn snapshot_texts<O: PullRequestObjects, P, E>(
    objects: &O,
    head: &[TreeEntry],
) -> Result<Vec<Vec<u8>>, CheckError<O::Error, P, E>> {
    let mut texts = Vec::new();
    for batch in batches(head) {
        let read = objects.read(&batch).map_err(CheckError::Objects)?;
        if read.len() != batch.len() {
            return Err(CheckError::ShortRead {
                asked: batch.len(),
                returned: read.len(),
            });
        }
        texts.extend(read);
    }
    Ok(texts)
}

/// The entries to index, grouped into reads of at most [`READ_BATCH_BYTES`].
fn batches(head: &[TreeEntry]) -> Vec<Vec<&TreeEntry>> {
    // `usize` is at most 64 bits wide on every target, so the conversion keeps the 64 MiB cap.
    let cap = u64::try_from(MAX_INDEXED_BYTES).unwrap_or(u64::MAX);
    let mut total: u64 = 0;
    let mut batches: Vec<Vec<&TreeEntry>> = Vec::new();
    let mut current: Vec<&TreeEntry> = Vec::new();
    let mut current_bytes: u64 = 0;
    for entry in head {
        let (EntryKind::File | EntryKind::Symlink, Some(size)) = (entry.kind, entry.size) else {
            continue;
        };
        let Some(next_total) = total.checked_add(size).filter(|&next| next <= cap) else {
            continue;
        };
        if size > READ_BATCH_BYTES {
            continue;
        }
        total = next_total;
        // Both are at most `READ_BATCH_BYTES`, so the sum never saturates.
        if current_bytes.saturating_add(size) > READ_BATCH_BYTES {
            batches.push(std::mem::take(&mut current));
            current_bytes = 0;
        }
        current_bytes = current_bytes.saturating_add(size);
        current.push(entry);
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, size: u64) -> TreeEntry {
        TreeEntry {
            path: path.to_owned(),
            kind: EntryKind::File,
            object: path.to_owned(),
            size: Some(size),
        }
    }

    fn paths<'a>(batches: &[Vec<&'a TreeEntry>]) -> Vec<Vec<&'a str>> {
        batches
            .iter()
            .map(|batch| batch.iter().map(|entry| entry.path.as_str()).collect())
            .collect()
    }

    #[test]
    fn a_batch_holds_exactly_its_cap_and_the_next_byte_starts_another() {
        let half = READ_BATCH_BYTES / 2;
        let head = [file("a", half), file("b", half), file("c", 1)];

        assert_eq!(paths(&batches(&head)), [vec!["a", "b"], vec!["c"]]);
    }

    #[test]
    fn a_file_larger_than_a_batch_and_a_submodule_are_left_out() {
        let submodule = TreeEntry {
            path: "vendor".to_owned(),
            kind: EntryKind::Submodule,
            object: "vendor".to_owned(),
            size: None,
        };
        let head = [
            file("big", READ_BATCH_BYTES + 1),
            file("fits", READ_BATCH_BYTES),
            submodule,
        ];

        assert_eq!(paths(&batches(&head)), [vec!["fits"]]);
    }

    #[test]
    fn files_past_the_index_cap_are_left_out_and_smaller_ones_still_fit() {
        let cap = u64::try_from(MAX_INDEXED_BYTES).expect("64 MiB fits in u64");
        let full = cap / READ_BATCH_BYTES;
        let mut head: Vec<TreeEntry> = (0..full)
            .map(|index| file(&format!("f{index:03}"), READ_BATCH_BYTES))
            .collect();
        head.push(file("over", 1));

        let indexed: u64 = batches(&head)
            .iter()
            .flatten()
            .filter_map(|entry| entry.size)
            .sum();

        assert_eq!(indexed, cap, "the cap is reached exactly");
        assert!(
            batches(&head)
                .iter()
                .flatten()
                .all(|entry| entry.path != "over"),
            "one byte past the cap is not indexed"
        );
    }
}
