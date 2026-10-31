//! `GET pulls/{n}/files`: the REST file list `plan` asserts git's list against (04 §2), with
//! the PR's `changed_files`.
//!
//! Worst case over PR-controlled input: the list is read through `Api::list`, so at most 30
//! pages of at most `MAX_RESPONSE_BYTES` each, refused past `MAX_LIST_ITEMS` (3,000) files.
//! Each file is converted once, and the repeated-path check is one pass inserting borrowed
//! paths into a `HashSet` sized for the list: O(n) expected inserts for n <= 3,000, each
//! hashing its path once (so O(total path bytes)) with std's randomly keyed `SipHash`, so a
//! PR cannot choose paths that collide. No path is compared with every other.

use std::collections::HashSet;
use std::num::NonZeroU64;

use assay_app::{REST_FILE_CAP, RestFile, RestFiles};
use serde::Deserialize;

use super::same_pull;
use crate::github::GitHub;
use crate::github::api::{MAX_LIST_ITEMS, PER_PAGE};
use crate::github::error::{ForgeError, Operation};
use crate::github::transport::{Sleep, Transport};

// Checked when the crate compiles. `list` must keep every file GitHub can list, or it would
// refuse a complete file list; and today it cuts no later than GitHub does, which is why the
// `REST_FILE_CAP` guard in `pull_files` cannot trigger (whoever raises `MAX_LIST_ITEMS` drops
// this line and relies on that guard).
const _: () = assert!(
    MAX_LIST_ITEMS >= REST_FILE_CAP,
    "list would refuse a complete PR file list"
);
const _: () = assert!(
    MAX_LIST_ITEMS <= REST_FILE_CAP,
    "pull_files' REST_FILE_CAP guard is now reachable: drop this assertion knowingly"
);

/// A PR's file count.
#[derive(Debug, Deserialize)]
struct ChangedFilesDto {
    number: u64,
    changed_files: u64,
}

/// One file of a PR's list.
#[derive(Debug, Deserialize)]
struct PullFileDto {
    filename: String,
    status: String,
    previous_filename: Option<String>,
}

impl<T: Transport, S: Sleep> GitHub<T, S> {
    /// PR `number`'s files as GitHub lists them, every page, with its `changed_files`. The
    /// list is not cut: GitHub lists at most 3,000 files, and `plan` tells a truncated list
    /// from `changed_files`.
    ///
    /// # Errors
    ///
    /// [`ForgeError::TooMany`] past 3,000 files; [`ForgeError::Malformed`] when GitHub answers
    /// for another PR, lists a path twice, a file without a name or with an unknown status, or
    /// a `previous_filename` that is empty, the file's own name, missing on a renamed or copied
    /// file or present on any other; the reads' errors.
    pub fn pull_files(&self, number: NonZeroU64) -> Result<RestFiles, ForgeError<T::Error>> {
        let pull_operation = Operation::ReadPull;
        let path = self.path(pull_operation, &format!("pulls/{number}"))?;
        let pull: ChangedFilesDto = self.api.get(pull_operation, &path)?;
        same_pull(pull_operation, number, pull.number)?;

        let operation = Operation::ListPullFiles;
        let path = self.path(
            operation,
            &format!("pulls/{number}/files?per_page={PER_PAGE}"),
        )?;
        let listed = self.api.list::<Vec<PullFileDto>>(operation, path, None)?;
        // `list` already refuses past `MAX_LIST_ITEMS`, equal to GitHub's cap today; this keeps
        // the file list within GitHub's cap should the general one ever grow.
        if listed.len() > REST_FILE_CAP {
            return Err(ForgeError::TooMany {
                operation,
                cap: REST_FILE_CAP,
            });
        }
        let files = listed
            .into_iter()
            .map(|file| rest_file(operation, file))
            .collect::<Result<Vec<_>, _>>()?;
        let mut seen = HashSet::with_capacity(files.len());
        if !files.iter().all(|file| seen.insert(file.path.as_str())) {
            return Err(ForgeError::Malformed {
                operation,
                detail: "a file is listed twice",
            });
        }
        Ok(RestFiles {
            files,
            changed_files: pull.changed_files,
        })
    }
}

/// One listed file, with `previous_filename` exactly when GitHub says it was renamed or copied.
fn rest_file<E>(operation: Operation, file: PullFileDto) -> Result<RestFile, ForgeError<E>> {
    let malformed = |detail| ForgeError::Malformed { operation, detail };
    if file.filename.is_empty() {
        return Err(malformed("a file has no filename"));
    }
    let moved = match file.status.as_str() {
        "renamed" | "copied" => true,
        "added" | "removed" | "modified" | "changed" | "unchanged" => false,
        _ => return Err(malformed("a file's status is not one GitHub documents")),
    };
    let previous_path = match (moved, file.previous_filename) {
        (true, None) => {
            return Err(malformed(
                "a renamed or copied file has no previous_filename",
            ));
        }
        (false, Some(_)) => {
            return Err(malformed(
                "only a renamed or copied file has a previous_filename",
            ));
        }
        (true, Some(previous)) if previous.is_empty() => {
            return Err(malformed("a previous_filename is empty"));
        }
        (true, Some(previous)) if previous == file.filename => {
            return Err(malformed("a previous_filename is the filename"));
        }
        (_, previous) => previous,
    };
    Ok(RestFile {
        path: file.filename,
        previous_path,
    })
}
