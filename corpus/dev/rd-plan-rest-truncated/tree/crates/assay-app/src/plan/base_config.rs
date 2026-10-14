//! Base `.assay/`, read at the pinned `base_sha` and never at head
//! (`docs/architecture/03-contracts.md` §1): what a PR changes in it applies from the next PR.

use assay_domain::{ContentHash, RepoGlobs};
use thiserror::Error;

use super::objects::{EntryKind, PullRequestObjects};
use super::{PlanError, read_entries};
use crate::config::{CommitId, ConfigError, GlobsConfigError, PluginsConfig, parse_globs};

/// The base configuration directory.
const CONFIG_DIR: &str = ".assay";
/// The gates and repository settings; required.
const PLUGINS: &str = ".assay/plugins.toml";
/// The repository's glob additions; optional, and absent adds nothing.
const GLOBS: &str = ".assay/globs.toml";

/// Why the base configuration was refused. It comes from the protected base, so messages may
/// name its paths.
#[derive(Debug, Error)]
pub enum BaseConfigError {
    /// The base has no `.assay/plugins.toml`: no gate is known, so nothing can be planned.
    #[error("the base has no {PLUGINS}")]
    Missing,
    /// An entry under `.assay/` is a symlink or submodule, which is refused rather than
    /// followed (wi-b5affa).
    #[error("{path} in the base is a {kind:?}, not a regular file")]
    NotARegularFile {
        /// The entry's path.
        path: String,
        /// What it is.
        kind: EntryKind,
    },
    /// A file `plan` parses is not UTF-8.
    #[error("{path} in the base is not UTF-8")]
    NotUtf8 {
        /// The file's path.
        path: String,
    },
    /// `.assay/plugins.toml` was refused.
    #[error("{PLUGINS} in the base was refused")]
    Plugins(#[source] ConfigError),
    /// `.assay/globs.toml` was refused.
    #[error("{GLOBS} in the base was refused")]
    Globs(#[source] GlobsConfigError),
}

/// What `plan` takes from base `.assay/`.
#[derive(Debug)]
pub(super) struct BaseConfig {
    pub(super) plugins: PluginsConfig,
    pub(super) globs: RepoGlobs,
    /// Every file under `.assay/`, path and bytes in path order: the evidence key's
    /// `config_hash` (02 §3), so any base config change makes evidence stale.
    pub(super) hash: ContentHash,
}

/// Reads every file under `.assay/` at `base`, refusing any entry that is not a regular file,
/// and parses `plugins.toml` and `globs.toml`.
pub(super) fn read<O: PullRequestObjects, N>(
    objects: &O,
    base: &CommitId,
) -> Result<BaseConfig, PlanError<O::Error, N>> {
    let entries = objects
        .list(base, Some(CONFIG_DIR))
        .map_err(PlanError::Objects)?;
    if let Some(entry) = entries.iter().find(|entry| entry.kind != EntryKind::File) {
        return Err(PlanError::BaseConfig(BaseConfigError::NotARegularFile {
            // The error owns the path it names.
            path: entry.path.clone(),
            kind: entry.kind,
        }));
    }
    let borrowed: Vec<_> = entries.iter().collect();
    let blobs = read_entries(objects, &borrowed)?;
    let files: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|entry| entry.path.as_str())
        .zip(blobs.iter().map(Vec::as_slice))
        .collect();
    let text = |wanted: &str| -> Result<Option<&str>, BaseConfigError> {
        files
            .iter()
            .find(|(path, _)| *path == wanted)
            .map(|(path, bytes)| {
                str::from_utf8(bytes).map_err(|_not_utf8| BaseConfigError::NotUtf8 {
                    path: (*path).to_owned(),
                })
            })
            .transpose()
    };
    let plugins = text(PLUGINS)
        .map_err(PlanError::BaseConfig)?
        .ok_or(PlanError::BaseConfig(BaseConfigError::Missing))?
        .parse()
        .map_err(|source| PlanError::BaseConfig(BaseConfigError::Plugins(source)))?;
    let globs = text(GLOBS)
        .map_err(PlanError::BaseConfig)?
        .map(parse_globs)
        .transpose()
        .map_err(|source| PlanError::BaseConfig(BaseConfigError::Globs(source)))?
        // No `globs.toml` adds nothing to the shipped lists (03 §1).
        .unwrap_or_default();
    Ok(BaseConfig {
        plugins,
        globs,
        hash: ContentHash::of_files(files.iter().copied()),
    })
}
