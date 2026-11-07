//! The dependency clusters of a change: disjoint sets over the changed files, joined along each
//! import of one changed file by another. Unchanged importers join nothing (a crate root
//! declares every module, so joining through it would make one cluster of the crate); they
//! are listed with each shard holding a file they import.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::{Import, ShardError, ShardFile};

/// Changed files grouped together, or a shard being filled with such groups.
#[derive(Debug, Default)]
pub(super) struct Cluster<'a> {
    /// Its changed files with their input positions.
    pub(super) files: Vec<(usize, &'a str)>,
    /// The pack bytes of its changed files.
    pub(super) pack_bytes: usize,
}

impl Cluster<'_> {
    /// Takes `other`'s files.
    pub(super) fn absorb(&mut self, other: Self) {
        self.files.extend(other.files);
        self.pack_bytes = self.pack_bytes.saturating_add(other.pack_bytes);
    }
}

/// The changed files as nodes (node `i` is input file `i`).
pub(super) struct Graph<'f, 'a> {
    files: &'f [ShardFile<'a>],
    nodes: HashMap<&'a str, usize>,
    sets: DisjointSets,
}

impl<'f, 'a> Graph<'f, 'a> {
    /// A graph of `files` with no edge yet.
    ///
    /// # Errors
    /// Returns [`ShardError::RepeatedFile`] when a path is listed twice.
    pub(super) fn new(files: &'f [ShardFile<'a>]) -> Result<Self, ShardError> {
        let mut nodes = HashMap::with_capacity(files.len());
        for (index, file) in files.iter().enumerate() {
            if nodes.insert(file.path, index).is_some() {
                return Err(ShardError::RepeatedFile {
                    path: file.path.to_owned(),
                });
            }
        }
        Ok(Self {
            files,
            nodes,
            sets: DisjointSets::new(files.len()),
        })
    }

    /// Joins the clusters of the two files when both changed; any other import joins nothing.
    pub(super) fn join(&mut self, import: Import<'_>) {
        let importer = self.nodes.get(import.importer).copied();
        let dependency = self.nodes.get(import.imported).copied();
        if let (Some(importer), Some(dependency)) = (importer, dependency) {
            self.sets.union(importer, dependency);
        }
    }

    /// The clusters, in the order of their first changed file, each file in input order.
    pub(super) fn clusters(mut self) -> Vec<Cluster<'a>> {
        // Each root's cluster is keyed by its first file, so the map iterates in that order.
        let mut first_of: HashMap<usize, usize> = HashMap::new();
        let mut clusters: BTreeMap<usize, Cluster<'a>> = BTreeMap::new();
        for (node, file) in self.files.iter().enumerate() {
            let root = self.sets.find(node);
            let first = *first_of.entry(root).or_insert(node);
            let cluster = clusters.entry(first).or_default();
            cluster.files.push((node, file.path));
            cluster.pack_bytes = cluster.pack_bytes.saturating_add(file.pack_bytes);
        }
        clusters.into_values().collect()
    }
}

/// The unchanged importers of each of `shards` shards, sorted and each listed once per shard:
/// for every import of a changed file (its shard in `shard_of`) by a file that did not change,
/// the importer goes with that shard. Linear in the imports (a hash set removes repeats), apart
/// from sorting each shard's list.
pub(super) fn importers<'a>(
    shard_of: &HashMap<&'a str, usize>,
    imports: &[Import<'a>],
    shards: usize,
) -> Vec<Vec<&'a str>> {
    let mut seen: HashSet<(usize, &'a str)> = HashSet::new();
    let pairs: Vec<(usize, &'a str)> = imports
        .iter()
        .filter(|import| !shard_of.contains_key(import.importer))
        .filter_map(|import| Some((*shard_of.get(import.imported)?, import.importer)))
        .filter(|&pair| seen.insert(pair))
        .collect();
    (0..shards)
        .map(|shard| {
            let mut paths: Vec<&'a str> = pairs
                .iter()
                .filter(|&&(of, _)| of == shard)
                .map(|&(_, path)| path)
                .collect();
            paths.sort_unstable();
            paths
        })
        .collect()
}

/// Union-find with union by size and path halving: near-constant time per operation, and no
/// recursion, so a long chain of imports cannot exhaust the stack.
struct DisjointSets {
    parent: Vec<usize>,
    size: Vec<usize>,
}

impl DisjointSets {
    fn new(len: usize) -> Self {
        Self {
            parent: (0..len).collect(),
            size: vec![1; len],
        }
    }

    /// The root of `node`'s set (`node` itself for a node out of range).
    fn find(&mut self, mut node: usize) -> usize {
        loop {
            let parent = self.parent.get(node).copied().unwrap_or(node);
            if parent == node {
                return node;
            }
            let grandparent = self.parent.get(parent).copied().unwrap_or(parent);
            if let Some(slot) = self.parent.get_mut(node) {
                *slot = grandparent;
            }
            node = grandparent;
        }
    }

    fn union(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a == b {
            return;
        }
        let size = |node: usize| self.size.get(node).copied().unwrap_or(1);
        let (small, large) = if size(a) < size(b) { (a, b) } else { (b, a) };
        let merged = size(small).saturating_add(size(large));
        if let Some(parent) = self.parent.get_mut(small) {
            *parent = large;
        }
        if let Some(slot) = self.size.get_mut(large) {
            *slot = merged;
        }
    }
}
