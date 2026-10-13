//! Sharding by dependency cluster (`docs/architecture/04-pipeline.md` §3, #98): a `full` PR
//! whose context pack exceeds [`MAX_DIFF_BYTES`] is split into at most [`MAX_SHARDS`] shards of
//! at most [`MAX_DIFF_BYTES`] each. A shard is a set of clusters (changed files joined where
//! one imports another), listed with the unchanged files that import them, so a file is
//! reviewed beside the code it uses and the code that uses it. Shards run one after another,
//! each with the full specialist set ([`run_shards`](crate::run_shards)), and the run cap
//! scales by their count ([`full_run_cap`]).
//!
//! The file list, the pack sizes and the import edges come from the pull request, so every step
//! is linear or `n log n` in them, never a loop over pairs, and the input is refused rather
//! than truncated (wi-b5affa): past a count cap, and when the clusters do not fit in the
//! shards, since a withheld file would be reported as reviewed.

mod cluster;

use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap};

use assay_domain::{Aeu, RequiredRole};

use self::cluster::{Cluster, Graph, importers};
use crate::check::MAX_DIFF_BYTES;
use crate::coordinate::COORDINATOR_CAP;
use crate::prompt_bundle::Role;
use crate::verify::VerifierTier;

/// The most shards a run is split into (04 §3).
pub const MAX_SHARDS: usize = 3;

/// The most changed files [`shard`] reads: `plan`'s cap on the REST file list.
pub const MAX_SHARD_FILES: usize = 3_000;

/// The most import edges [`shard`] reads.
pub const MAX_IMPORTS: usize = 1 << 16;

/// A changed file and the bytes it takes in the context pack (its diff, and the base side of
/// deleted code).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShardFile<'a> {
    /// The file's repository path.
    pub path: &'a str,
    /// Its bytes in the context pack.
    pub pack_bytes: usize,
}

/// One file of the head tree importing another (`use`, `mod`, or what the caller's language
/// reads as a dependency). Only imports of changed files matter; the rest are ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Import<'a> {
    /// The file that imports.
    pub importer: &'a str,
    /// The file it imports.
    pub imported: &'a str,
}

/// How many shards a run has: one to [`MAX_SHARDS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ShardCount(u8);

impl ShardCount {
    /// An unsharded run.
    pub const ONE: Self = Self(1);

    /// `count` shards, if it is one to [`MAX_SHARDS`].
    #[must_use]
    pub fn new(count: usize) -> Option<Self> {
        if (1..=MAX_SHARDS).contains(&count) {
            u8::try_from(count).ok().map(Self)
        } else {
            None
        }
    }

    /// The number of shards.
    #[must_use]
    pub fn get(self) -> usize {
        usize::from(self.0)
    }
}

/// A run's shards, one to [`MAX_SHARDS`], heaviest cluster first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sharding<'a> {
    shards: Vec<Shard<'a>>,
    count: ShardCount,
}

impl<'a> Sharding<'a> {
    /// The shards, in the order they run.
    #[must_use]
    pub fn shards(&self) -> &[Shard<'a>] {
        &self.shards
    }

    /// How many shards there are.
    #[must_use]
    pub const fn count(&self) -> ShardCount {
        self.count
    }
}

/// One shard: the changed files its pack shows and the unchanged files importing them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shard<'a> {
    files: Vec<&'a str>,
    importers: Vec<&'a str>,
    pack_bytes: usize,
}

impl<'a> Shard<'a> {
    /// The changed files, in the order they were listed.
    #[must_use]
    pub fn files(&self) -> &[&'a str] {
        &self.files
    }

    /// The unchanged files that directly import one of [`files`](Self::files), sorted; the
    /// reviewers read them from the tree. An importer of files in several shards is listed with
    /// each. Empty for an unsharded run.
    #[must_use]
    pub fn importers(&self) -> &[&'a str] {
        &self.importers
    }

    /// The pack bytes of its files.
    #[must_use]
    pub const fn pack_bytes(&self) -> usize {
        self.pack_bytes
    }
}

/// Why [`shard`] refused its input.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ShardError {
    /// No changed file was given: there is nothing to review.
    #[error("no changed file to shard")]
    NoFiles,
    /// More than [`MAX_SHARD_FILES`] changed files.
    #[error("{files} changed files; at most {MAX_SHARD_FILES} are sharded")]
    TooManyFiles {
        /// How many were given.
        files: usize,
    },
    /// More than [`MAX_IMPORTS`] import edges.
    #[error("{imports} imports; at most {MAX_IMPORTS} are read")]
    TooManyImports {
        /// How many were given.
        imports: usize,
    },
    /// The clusters do not fit in [`MAX_SHARDS`] shards of [`MAX_DIFF_BYTES`]: a cluster is
    /// larger than a shard, or none of the shards open has room for it. Reviewing the rest would
    /// report the withheld files as reviewed, so nothing is sharded.
    #[error("{files} changed files ({withheld_bytes} pack bytes) fit in no shard")]
    Overfull {
        /// The pack bytes of the clusters that fitted nowhere.
        withheld_bytes: usize,
        /// How many changed files they hold.
        files: usize,
    },
    /// A changed file was listed twice, so its pack bytes are ambiguous.
    #[error("the changed file {path:?} is listed twice")]
    RepeatedFile {
        /// The repeated path.
        path: String,
    },
}

/// Splits `files` into shards by dependency cluster (04 §3).
///
/// A pack of at most [`MAX_DIFF_BYTES`] is one shard of every file, in order. A larger one is
/// clustered: two changed files share a cluster when one imports the other in `imports`,
/// directly or through other changed files. The clusters, heaviest first (ties in the order of
/// their first file), go to the first shard with room, or to a new one while fewer than
/// [`MAX_SHARDS`] are open. Every changed file is in exactly one shard, and every unchanged
/// file importing one is listed with that shard.
///
/// Linear in the files and imports apart from sorting (`n log n`); no step compares pairs.
///
/// # Errors
/// Returns [`ShardError`] for no file, more than [`MAX_SHARD_FILES`] files, more than
/// [`MAX_IMPORTS`] imports, a file listed twice, or clusters that do not fit in the shards
/// ([`Overfull`](ShardError::Overfull)); nothing is sharded then.
///
/// # Examples
///
/// ```
/// use assay_app::{Import, MAX_DIFF_BYTES, ShardFile, shard};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let files = [
///     ShardFile { path: "src/a.rs", pack_bytes: MAX_DIFF_BYTES },
///     ShardFile { path: "src/b.rs", pack_bytes: 100 },
/// ];
/// let imports = [Import { importer: "src/lib.rs", imported: "src/b.rs" }];
///
/// let sharding = shard(&files, &imports)?;
///
/// let [first, second] = sharding.shards() else {
///     return Err("expected two shards".into());
/// };
/// assert_eq!((first.files(), first.importers()), (&["src/a.rs"][..], &[][..]));
/// assert_eq!((second.files(), second.importers()), (&["src/b.rs"][..], &["src/lib.rs"][..]));
/// # Ok(())
/// # }
/// ```
pub fn shard<'a>(
    files: &[ShardFile<'a>],
    imports: &[Import<'a>],
) -> Result<Sharding<'a>, ShardError> {
    if files.is_empty() {
        return Err(ShardError::NoFiles);
    }
    if files.len() > MAX_SHARD_FILES {
        return Err(ShardError::TooManyFiles { files: files.len() });
    }
    if imports.len() > MAX_IMPORTS {
        return Err(ShardError::TooManyImports {
            imports: imports.len(),
        });
    }
    let mut graph = Graph::new(files)?;
    let total = files
        .iter()
        .fold(0_usize, |sum, file| sum.saturating_add(file.pack_bytes));
    if total <= MAX_DIFF_BYTES {
        let shard = Shard {
            files: files.iter().map(|file| file.path).collect(),
            importers: Vec::new(),
            pack_bytes: total,
        };
        return Ok(Sharding {
            shards: vec![shard],
            count: ShardCount::ONE,
        });
    }
    for &import in imports {
        graph.join(import);
    }
    let bins = pack(graph.clusters())?;
    // Every file was placed (or `pack` refused), and there is at least one file.
    let count = ShardCount::new(bins.len()).ok_or(ShardError::NoFiles)?;
    let shard_of: HashMap<&'a str, usize> = bins
        .iter()
        .enumerate()
        .flat_map(|(shard, bin)| bin.files.iter().map(move |&(_, path)| (path, shard)))
        .collect();
    let shards = bins
        .into_iter()
        .zip(importers(&shard_of, imports, count.get()))
        .map(|(mut bin, importers)| {
            bin.files.sort_unstable();
            Shard {
                files: bin.files.into_iter().map(|(_, path)| path).collect(),
                importers,
                pack_bytes: bin.pack_bytes,
            }
        })
        .collect();
    Ok(Sharding { shards, count })
}

/// First-fit decreasing into at most [`MAX_SHARDS`] shards of [`MAX_DIFF_BYTES`]: linear in the
/// clusters after the sort, which is stable, so equal clusters keep the order of their first
/// file.
///
/// # Errors
/// Returns [`ShardError::Overfull`] with every cluster that fitted nowhere.
fn pack(mut clusters: Vec<Cluster<'_>>) -> Result<Vec<Cluster<'_>>, ShardError> {
    clusters.sort_by_key(|cluster| Reverse(cluster.pack_bytes));
    let mut bins: Vec<Cluster<'_>> = Vec::with_capacity(MAX_SHARDS);
    let (mut withheld_bytes, mut withheld_files) = (0_usize, 0_usize);
    for cluster in clusters {
        let room = |bin: &&mut Cluster<'_>| {
            bin.pack_bytes.saturating_add(cluster.pack_bytes) <= MAX_DIFF_BYTES
        };
        if let Some(bin) = bins.iter_mut().find(room) {
            bin.absorb(cluster);
        } else if cluster.pack_bytes <= MAX_DIFF_BYTES && bins.len() < MAX_SHARDS {
            bins.push(cluster);
        } else {
            withheld_bytes = withheld_bytes.saturating_add(cluster.pack_bytes);
            withheld_files = withheld_files.saturating_add(cluster.files.len());
        }
    }
    if withheld_files > 0 {
        return Err(ShardError::Overfull {
            withheld_bytes,
            files: withheld_files,
        });
    }
    Ok(bins)
}

/// The run cap of a `full` run with `roles` (each counted once) split into `shards`.
///
/// Per shard it is the roles' caps, the coordinator's and the verifier stage's, so $2.30 a
/// shard ($2.60 with `meta`), and $6.90 ($7.80) for three (04 §3, 06 §1).
#[must_use]
pub fn full_run_cap(roles: &[RequiredRole], shards: ShardCount) -> Aeu {
    let per_shard = roles
        .iter()
        .copied()
        .collect::<BTreeSet<RequiredRole>>()
        .into_iter()
        .map(|role| Role::from(role).cap())
        .fold(
            COORDINATOR_CAP.saturating_add(VerifierTier::Full.budget()),
            Aeu::saturating_add,
        );
    Aeu::from_micros(per_shard.micros().saturating_mul(u64::from(shards.0)))
}
