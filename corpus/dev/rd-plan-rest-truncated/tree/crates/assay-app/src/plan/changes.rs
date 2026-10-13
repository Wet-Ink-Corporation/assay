//! The tiering inputs of one PR (`docs/architecture/04-pipeline.md` §2.2): manifests and the
//! temper baseline read at both SHAs, and the exec surface from the package sets.

use std::collections::{BTreeMap, BTreeSet};

use assay_domain::{BaselineChange, ChangeSet, FileChange, ManifestTrustChange};

use super::objects::{EntryKind, PullRequestObjects};
use super::{IncludeTargets, PlanError, PlanRequest, read_entries};
use crate::config::CommitId;
use crate::tiering::{baseline_change, exec_surface, manifest_exec_roots, manifest_trust_keys};

/// TOML that never parses: what a manifest reads as when it is a symlink, a submodule or not
/// UTF-8, so it gets exactly the fail-closed treatment of an unparseable one (cargo would
/// follow the symlink or refuse the file; `plan` does neither).
const UNREADABLE_MANIFEST: &str = "[";

/// The temper ratchet file (04 §2.2).
const TEMPER_BASELINE: &str = ".claude/temper-baseline.json";

/// Manifest texts by path at one commit.
type Manifests = BTreeMap<String, String>;

/// The tiering input for `files`: trust keys of every changed manifest, the exec roots of
/// every manifest at either commit with the package-set diff, the include targets and the
/// temper baseline's change.
pub(super) fn change_set<O: PullRequestObjects, N>(
    objects: &O,
    request: &PlanRequest,
    files: Vec<FileChange>,
) -> Result<ChangeSet, PlanError<O::Error, N>> {
    let pull_request = &request.pull_request;
    let base = manifests(objects, &pull_request.base_sha)?;
    let head = manifests(objects, &pull_request.head_sha)?;
    let paths: BTreeSet<&str> = base.keys().chain(head.keys()).map(String::as_str).collect();
    let declared: Vec<_> = paths
        .iter()
        .flat_map(|path| manifest_exec_roots(path, text(&base, path), text(&head, path)))
        .collect();
    let surface = exec_surface(
        request.base_packages.as_ref(),
        request.head_packages.as_ref(),
        &declared,
    );

    let changed: BTreeSet<&str> = files.iter().flat_map(FileChange::sides).collect();
    let manifest_changes = changed
        .iter()
        .filter(|path| is_manifest(path))
        .flat_map(|path| {
            manifest_trust_keys(text(&base, path), text(&head, path))
                .into_iter()
                .map(|key| ManifestTrustChange {
                    path: (*path).to_owned(),
                    key,
                })
        })
        .collect();
    let include_targets = match &request.include_targets {
        // The change set owns its targets; the request keeps its own.
        IncludeTargets::Scanned(targets) => targets.clone(),
        IncludeTargets::NotScanned => changed.iter().map(|path| (*path).to_owned()).collect(),
    };
    let temper_baseline = if changed.contains(TEMPER_BASELINE) {
        baseline(objects, request)?
    } else {
        BaselineChange::Other
    };
    Ok(ChangeSet {
        files,
        manifests: manifest_changes,
        include_targets,
        exec_roots: surface.exec_roots,
        packages: surface.packages,
        temper_baseline,
    })
}

/// The text of the manifest at `path` on one side; `None` where there is none.
fn text<'a>(side: &'a Manifests, path: &str) -> Option<&'a str> {
    side.get(path).map(String::as_str)
}

/// Whether `path` names a cargo manifest, ignoring ASCII case (fail closed: a `cargo.toml`
/// is read as one too).
fn is_manifest(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .is_some_and(|name| name.eq_ignore_ascii_case("Cargo.toml"))
}

/// Every manifest in `commit`'s tree, by path.
fn manifests<O: PullRequestObjects, N>(
    objects: &O,
    commit: &CommitId,
) -> Result<Manifests, PlanError<O::Error, N>> {
    let mut found = objects.list(commit, None).map_err(PlanError::Objects)?;
    found.retain(|entry| is_manifest(&entry.path));
    let readable: Vec<_> = found
        .iter()
        .filter(|entry| entry.kind == EntryKind::File)
        .collect();
    let mut blobs = read_entries(objects, &readable)?.into_iter();
    Ok(found
        .into_iter()
        .map(|entry| {
            let text = if entry.kind == EntryKind::File {
                blobs.next().and_then(utf8)
            } else {
                None
            };
            let text = text.unwrap_or_else(|| UNREADABLE_MANIFEST.to_owned());
            (entry.path, text)
        })
        .collect())
}

/// `bytes` as text, or `None` when they are not UTF-8: the caller reads that as unreadable,
/// never as an absent file.
fn utf8(bytes: Vec<u8>) -> Option<String> {
    String::from_utf8(bytes).ok()
}

/// How the temper baseline changed between the base and head commits.
fn baseline<O: PullRequestObjects, N>(
    objects: &O,
    request: &PlanRequest,
) -> Result<BaselineChange, PlanError<O::Error, N>> {
    let pull_request = &request.pull_request;
    let base = file_text(objects, &pull_request.base_sha, TEMPER_BASELINE)?;
    let head = file_text(objects, &pull_request.head_sha, TEMPER_BASELINE)?;
    Ok(baseline_change(base.as_deref(), head.as_deref()))
}

/// The text of the regular file at exactly `path` in `commit`; `None` when there is none, or
/// it is not a regular UTF-8 file (which `baseline_change` reads as any other change).
fn file_text<O: PullRequestObjects, N>(
    objects: &O,
    commit: &CommitId,
    path: &str,
) -> Result<Option<String>, PlanError<O::Error, N>> {
    let entries = objects
        .list(commit, Some(path))
        .map_err(PlanError::Objects)?;
    let Some(entry) = entries
        .iter()
        .find(|entry| entry.path == path && entry.kind == EntryKind::File)
    else {
        return Ok(None);
    };
    let blob = read_entries(objects, &[entry])?.into_iter().next();
    Ok(blob.and_then(utf8))
}
