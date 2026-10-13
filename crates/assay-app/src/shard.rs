//! Sharding by dependency cluster (`docs/architecture/04-pipeline.md` §3, #98): a `full` PR
//! whose context pack exceeds [`MAX_DIFF_BYTES`] is split into at most [`MAX_SHARDS`] shards,
//! each a set of clusters (a changed file, its direct importers, and every changed file
//! reachable through them), so a file is reviewed beside the code that uses it. Shards run one
//! after another, each with the full specialist set ([`run_shards`](crate::run_shards)), and
//! the run cap scales by their count ([`full_run_cap`]).
//!
//! The file list, the pack sizes and the import edges come from the pull request, so every step
//! is linear or `n log n` in them, never a loop over pairs, and the counts are capped: past a
//! cap the input is refused, not truncated (wi-b5affa).

mod cluster;

use std::cmp::Reverse;
use std::collections::BTreeSet;

use assay_domain::{Aeu, RequiredRole};

use self::cluster::{Cluster, Graph};
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
}

impl<'a> Sharding<'a> {
    /// The shards, in the order they run.
    #[must_use]
    pub fn shards(&self) -> &[Shard<'a>] {
        &self.shards
    }

    /// How many shards there are.
    #[must_use]
    pub fn count(&self) -> ShardCount {
        // `shard` builds one to `MAX_SHARDS`, so the fallback is never taken.
        ShardCount::new(self.shards.len()).unwrap_or(ShardCount::ONE)
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
    /// reviewers read them from the tree. Empty for an unsharded run.
    #[must_use]
    pub fn importers(&self) -> &[&'a str] {
        &self.importers
    }

    /// The pack bytes of its files.
    #[must_use]
    pub const fn pack_bytes(&self) -> usize {
        self.pack_bytes
    }

    /// Whether its files take more than [`MAX_DIFF_BYTES`]: a cluster larger than that, or one
    /// that fitted in no shard once all three were open. Its pack shows what fits and says how
    /// many files it left out, as an unsharded pack does.
    #[must_use]
    pub const fn is_overfull(&self) -> bool {
        self.pack_bytes > MAX_DIFF_BYTES
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
    /// A changed file was listed twice, so its pack bytes are ambiguous.
    #[error("the changed file {path:?} is listed twice")]
    RepeatedFile {
        /// The repeated path.
        path: String,
    },
}

/// Splits `files` into shards by dependency cluster (04 §3).
///
/// A pack of at most [`MAX_DIFF_BYTES`] is one shard of every file, in order. A larger one is clustered: each changed file joins its direct
/// importers in `imports`, and clusters that share a file merge. The clusters, heaviest first
/// (ties in the order of their first file), go to the first shard they fit in, to a new one
/// while fewer than [`MAX_SHARDS`] are open, and otherwise to the lightest, which is then
/// [overfull](Shard::is_overfull). Every changed file is in exactly one shard.
///
/// Linear in the files and imports apart from sorting (`n log n`); no step compares pairs.
///
/// # Errors
/// Returns [`ShardError`] for no file, more than [`MAX_SHARD_FILES`] files, more than
/// [`MAX_IMPORTS`] imports, or a file listed twice; nothing is sharded then.
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
        });
    }
    for &import in imports {
        graph.join(import);
    }
    Ok(Sharding {
        shards: pack(files, graph.clusters()),
    })
}

/// First-fit decreasing into at most [`MAX_SHARDS`] shards: linear in the clusters after the
/// sort, which is stable, so equal clusters keep the order of their first file.
fn pack<'a>(files: &[ShardFile<'a>], mut clusters: Vec<Cluster<'a>>) -> Vec<Shard<'a>> {
    clusters.sort_by_key(|cluster| Reverse(cluster.pack_bytes));
    let mut bins: Vec<Cluster<'a>> = Vec::with_capacity(MAX_SHARDS);
    for cluster in clusters {
        let fits = bins
            .iter()
            .position(|bin| bin.pack_bytes.saturating_add(cluster.pack_bytes) <= MAX_DIFF_BYTES);
        let target = match fits {
            Some(index) => bins.get_mut(index),
            None if bins.len() < MAX_SHARDS => {
                bins.push(Cluster::default());
                bins.last_mut()
            }
            // The first of the lightest, so ties keep shard order.
            None => bins.iter_mut().min_by_key(|bin| bin.pack_bytes),
        };
        if let Some(bin) = target {
            bin.absorb(cluster);
        }
    }
    bins.into_iter()
        .map(|mut bin| {
            bin.files.sort_unstable();
            bin.importers.sort_unstable();
            Shard {
                files: bin
                    .files
                    .iter()
                    .filter_map(|&index| files.get(index).map(|file| file.path))
                    .collect(),
                importers: bin.importers,
                pack_bytes: bin.pack_bytes,
            }
        })
        .collect()
}

/// The run cap of a `full` run with `roles` (each counted once) split into `shards`.
///
/// Per shard it is the roles' caps, the coordinator's and the verifier stage's, so $2.30 a shard ($2.60 with
/// `meta`), and $6.90 ($7.80) for three (04 §3, 06 §1).
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
