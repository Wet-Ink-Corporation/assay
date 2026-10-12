//! The path crates that run at build time (the #50 review's F4): reached from a
//! build-dependency edge or a proc-macro crate, transitively.

use std::collections::BTreeSet;

use assay_domain::ExecRoot;

use super::{Location, PackageSet, Source};

/// The build-time path crates of one or both sides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum BuildTime {
    /// Their directories, relative to the copy's root, none of them the root.
    Dirs(BTreeSet<String>),
    /// The root, a directory outside the copy, or a side without a resolve graph.
    Everything,
}

impl BuildTime {
    /// The build-time crates of either side.
    pub(super) fn union(self, other: Self) -> Self {
        match (self, other) {
            (Self::Dirs(mut dirs), Self::Dirs(more)) => {
                dirs.extend(more);
                Self::Dirs(dirs)
            }
            (Self::Everything, _) | (_, Self::Everything) => Self::Everything,
        }
    }

    /// As exec roots, sorted.
    pub(super) fn into_roots(self) -> Vec<ExecRoot> {
        match self {
            Self::Dirs(dirs) => dirs.into_iter().map(ExecRoot::Path).collect(),
            Self::Everything => vec![ExecRoot::Everything],
        }
    }
}

/// The path crates of `set` that run at build time: each proc-macro crate and each target of
/// a build edge, then everything they reach over normal and build edges.
pub(super) fn build_time(set: &PackageSet) -> BuildTime {
    let Some(graph) = &set.graph else {
        return BuildTime::Everything;
    };
    let mut pending: Vec<usize> = Vec::new();
    for (at, package) in set.packages.iter().enumerate() {
        if package.proc_macro {
            pending.push(at);
        }
        let edges = graph.get(at).map_or(&[][..], Vec::as_slice);
        pending.extend(edges.iter().filter(|edge| edge.build).map(|edge| edge.to));
    }
    let mut reached = vec![false; set.packages.len()];
    let mut dirs = BTreeSet::new();
    while let Some(at) = pending.pop() {
        let Some(seen) = reached.get_mut(at) else {
            return BuildTime::Everything;
        };
        if *seen {
            continue;
        }
        *seen = true;
        match set.packages.get(at).map(|package| &package.source) {
            Some(Source::Path(Location::Inside(dir))) if !dir.is_empty() => {
                // The set keeps its package; the root owns a copy of the directory.
                dirs.insert(dir.clone());
            }
            Some(Source::Path(_)) | None => return BuildTime::Everything,
            Some(Source::Other(_)) => {}
        }
        let edges = graph.get(at).map_or(&[][..], Vec::as_slice);
        pending.extend(edges.iter().map(|edge| edge.to));
    }
    BuildTime::Dirs(dirs)
}
