//! The dependency clusters of a change: disjoint sets over the changed files and their direct
//! importers, joined along each import of a changed file.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use super::{Import, ShardError, ShardFile};

/// Changed files grouped with their importers, or a shard being filled with such groups.
#[derive(Debug, Default)]
pub(super) struct Cluster<'a> {
    /// Indices of its changed files in the input.
    pub(super) files: Vec<usize>,
    /// Its unchanged importers.
    pub(super) importers: Vec<&'a str>,
    /// The pack bytes of its changed files.
    pub(super) pack_bytes: usize,
}

impl Cluster<'_> {
    /// Takes `other`'s files and importers.
    pub(super) fn absorb(&mut self, other: Self) {
        self.files.extend(other.files);
        self.importers.extend(other.importers);
        self.pack_bytes = self.pack_bytes.saturating_add(other.pack_bytes);
    }
}

/// The files as nodes, changed ones first (node `i` is input file `i`), then each unchanged
/// importer as an import first names it.
pub(super) struct Graph<'f, 'a> {
    files: &'f [ShardFile<'a>],
    nodes: HashMap<&'a str, usize>,
    /// The path of each node past the changed files.
    importers: Vec<&'a str>,
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
            importers: Vec::new(),
            sets: DisjointSets::new(files.len()),
        })
    }

    /// Joins the importer to the cluster of the file it imports, when that file changed; an
    /// import of an unchanged file, or of a file by itself, joins nothing.
    pub(super) fn join(&mut self, import: Import<'a>) {
        if import.importer == import.imported {
            return;
        }
        let dependency = match self.nodes.get(import.imported) {
            Some(&node) if node < self.files.len() => node,
            _ => return,
        };
        let importer = match self.nodes.entry(import.importer) {
            Entry::Occupied(node) => *node.get(),
            Entry::Vacant(vacant) => {
                let node = self.sets.add();
                self.importers.push(import.importer);
                *vacant.insert(node)
            }
        };
        self.sets.union(dependency, importer);
    }

    /// The clusters, in the order of their first changed file. Every unchanged importer was
    /// joined to a changed file when it was added, so its cluster is one of these.
    pub(super) fn clusters(mut self) -> Vec<Cluster<'a>> {
        let mut cluster_of: Vec<Option<usize>> = vec![None; self.sets.len()];
        let mut clusters: Vec<Cluster<'a>> = Vec::new();
        for (node, file) in self.files.iter().enumerate() {
            let root = self.sets.find(node);
            let Some(slot) = cluster_of.get_mut(root) else {
                continue;
            };
            let index = *slot.get_or_insert_with(|| {
                clusters.push(Cluster::default());
                clusters.len().saturating_sub(1)
            });
            if let Some(cluster) = clusters.get_mut(index) {
                cluster.files.push(node);
                cluster.pack_bytes = cluster.pack_bytes.saturating_add(file.pack_bytes);
            }
        }
        let changed = self.files.len();
        for (offset, &path) in self.importers.iter().enumerate() {
            let root = self.sets.find(changed.saturating_add(offset));
            let cluster = cluster_of
                .get(root)
                .copied()
                .flatten()
                .and_then(|index| clusters.get_mut(index));
            if let Some(cluster) = cluster {
                cluster.importers.push(path);
            }
        }
        clusters
    }
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

    const fn len(&self) -> usize {
        self.parent.len()
    }

    /// A new singleton set, by its node.
    fn add(&mut self) -> usize {
        let node = self.parent.len();
        self.parent.push(node);
        self.size.push(1);
        node
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
