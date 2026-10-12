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
        let Some(package) = set.packages.get(at) else {
            return BuildTime::Everything;
        };
        match &package.source {
            Source::Path(Location::Inside(dir)) if !dir.is_empty() => {
                // The set keeps its package; the root owns a copy of the directory.
                dirs.insert(dir.clone());
                if !add_libraries(dir, &package.libraries, &mut dirs) {
                    return BuildTime::Everything;
                }
            }
            Source::Path(_) => return BuildTime::Everything,
            Source::Other(_) => {}
        }
        let edges = graph.get(at).map_or(&[][..], Vec::as_slice);
        pending.extend(edges.iter().map(|edge| edge.to));
    }
    BuildTime::Dirs(dirs)
}

/// Adds to `roots` each library source of the build-time crate in `dir` that lies outside
/// `dir` (`[lib] path`, #65 review round 2 F1): the file and its directory, which holds the
/// modules it declares. `false` when one is outside the copy or directly in its root, which
/// only [`BuildTime::Everything`] covers.
fn add_libraries(dir: &str, libraries: &[Location], roots: &mut BTreeSet<String>) -> bool {
    for library in libraries {
        let Location::Inside(file) = library else {
            return false;
        };
        let inside_crate = file
            .strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'));
        if inside_crate {
            continue;
        }
        let Some((parent, _)) = file.rsplit_once('/') else {
            return false;
        };
        // The set keeps its package; the roots own copies of the paths.
        roots.insert(parent.to_owned());
        roots.insert(file.clone());
    }
    true
}
