//! The exec roots of one change set, indexed so that whether any of them holds a path costs
//! that path's length, whatever the number of roots (#186): never paths times roots.

use std::collections::HashMap;

use super::change::{ExecRoot, crate_build_source, is_canonical, root_build_source};

/// Every exec root of a change set. [`Self::holds`] agrees with
/// `roots.iter().any(|root| root.holds(path))`.
///
/// The canonical `Path` roots and `BuildSources` directories form a tree of ASCII-lowercased
/// segments, kept flat (an edge map and a node list) so that no depth of root recurses.
#[derive(Debug)]
pub(super) struct RootIndex {
    /// A root holds every path: `Everything`, or a root that is not canonical (fail closed).
    everything: bool,
    /// The root package's `BuildSources` (`crate_dir` empty).
    root_package: bool,
    /// For each lowercased segment, the node it leads to from each node it follows. Node 0 is
    /// the repository root.
    edges: HashMap<String, HashMap<usize, usize>>,
    /// What ends at each node, by node.
    ends: Vec<Ends>,
}

/// The roots that end at one node.
#[derive(Debug, Clone, Copy, Default)]
struct Ends {
    /// An `ExecRoot::Path`: it and everything under it.
    path: bool,
    /// An `ExecRoot::BuildSources` crate directory.
    crate_dir: bool,
}

impl RootIndex {
    /// The index of `roots`, in one pass over their bytes.
    pub(super) fn new(roots: &[ExecRoot]) -> Self {
        let mut index = Self {
            everything: false,
            root_package: false,
            edges: HashMap::new(),
            ends: vec![Ends::default()],
        };
        for root in roots {
            match root {
                ExecRoot::Path(path) if is_canonical(path) => {
                    if let Some(ends) = index.insert(path) {
                        ends.path = true;
                    }
                }
                ExecRoot::BuildSources { crate_dir } if crate_dir.is_empty() => {
                    index.root_package = true;
                }
                ExecRoot::BuildSources { crate_dir } if is_canonical(crate_dir) => {
                    if let Some(ends) = index.insert(crate_dir) {
                        ends.crate_dir = true;
                    }
                }
                ExecRoot::Path(_) | ExecRoot::BuildSources { .. } | ExecRoot::Everything => {
                    index.everything = true;
                }
            }
        }
        index
    }

    /// The node `dir`'s segments lead to, added with the nodes on the way when missing.
    fn insert(&mut self, dir: &str) -> Option<&mut Ends> {
        let mut node = 0;
        for segment in dir.to_ascii_lowercase().split('/') {
            let fresh = self.ends.len();
            // The edge map stores its key; a segment seen before keeps its first copy.
            let child = *self
                .edges
                .entry(segment.to_owned())
                .or_default()
                .entry(node)
                .or_insert(fresh);
            if child == fresh {
                self.ends.push(Ends::default());
            }
            node = child;
        }
        self.ends.get_mut(node)
    }

    /// Whether some root holds `path`, ignoring ASCII case: one walk down its segments, each
    /// looked up once.
    pub(super) fn holds(&self, path: &str) -> bool {
        if self.everything || (self.root_package && root_build_source(path)) {
            return true;
        }
        let lower = path.to_ascii_lowercase();
        let mut node = 0;
        let mut rest = lower.as_str();
        loop {
            let (segment, after) = rest
                .split_once('/')
                .map_or((rest, None), |(segment, after)| (segment, Some(after)));
            let Some(&child) = self.edges.get(segment).and_then(|from| from.get(&node)) else {
                return false;
            };
            // `crate_build_source` reads only the suffix and the first segment of `after`, so
            // the walk stays linear in the path.
            let held = self.ends.get(child).is_some_and(|ends| {
                ends.path || (ends.crate_dir && after.is_some_and(crate_build_source))
            });
            if held {
                return true;
            }
            let Some(after) = after else {
                return false;
            };
            (node, rest) = (child, after);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_agrees_with_each_root_on_every_case() {
        let roots = [
            ExecRoot::Path("crates/a".to_owned()),
            ExecRoot::Path("Tools/Gen.rs".to_owned()),
            ExecRoot::BuildSources {
                crate_dir: "crates/b".to_owned(),
            },
            ExecRoot::BuildSources {
                crate_dir: "crates/b/inner".to_owned(),
            },
            ExecRoot::BuildSources {
                crate_dir: String::new(),
            },
        ];
        let paths = [
            "crates/a",
            "CRATES/A/src/lib.rs",
            "crates/ab/x.rs",
            "crates",
            "tools/gen.RS",
            "tools/gen.rs/x",
            "crates/b/build/gen.rs",
            "crates/b/src/lib.rs",
            "crates/b/inner/src/x.rs",
            "crates/B/Inner/codegen.rs",
            "crates/b/x.md",
            "crates/b",
            "crates/b//x.rs",
            "lib.rs",
            "a/mod.rs",
            "a/b/mod.rs",
            "",
            "/crates/a",
        ];
        let index = RootIndex::new(&roots);

        let disagreements: Vec<&str> = paths
            .into_iter()
            .filter(|path| index.holds(path) != roots.iter().any(|root| root.holds(path)))
            .collect();

        assert_eq!(disagreements, Vec::<&str>::new());
    }

    #[test]
    fn a_root_that_is_not_canonical_or_everything_holds_every_path() {
        let held: Vec<bool> = [
            ExecRoot::Everything,
            ExecRoot::Path(String::new()),
            ExecRoot::Path("a/../b".to_owned()),
            ExecRoot::BuildSources {
                crate_dir: "a//b".to_owned(),
            },
        ]
        .into_iter()
        .map(|root| RootIndex::new(&[root]).holds("docs/x.md"))
        .collect();

        assert_eq!(held, [true; 4]);
    }
}
