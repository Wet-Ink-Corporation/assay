//! Properties of tiering over generated change sets and repo globs: a repository's globs only
//! add, so a risk glob never lowers a tier, no addition moves a trust path below `full`, code
//! is never `trivial`, a rename tiers the same whichever side is the head, and the exec surface
//! is exactly the sides some exec root holds.

use std::collections::BTreeSet;

use assay_domain::{ExecRoot, FileChange, Glob, Lines, RepoGlobs, Tier, tier};
use proptest::collection::vec;
use proptest::prelude::*;
use proptest::sample::select;

use crate::support::changes;

/// Paths that exercise every list: docs, code, trust paths, risk globs, exclusions, vendor.
const PATHS: [&str; 16] = [
    "README.md",
    "docs/guide.md",
    "docs/img/a.png",
    "src/lib.rs",
    "src/auth/login.rs",
    "build.rs",
    ".github/workflows/ci.yml",
    "AGENTS.md",
    "Cargo.toml",
    "Cargo.lock",
    "static/app.min.js",
    "tests/out.snap",
    "vendor/zlib/inflate.c",
    "web/index.html",
    "skills/x/SKILL.md",
    "gen/schema.json",
];

/// Patterns a repository might add to any list.
const PATTERNS: [&str; 10] = [
    "**",
    "**/*.md",
    "**/*.rs",
    "docs/**",
    "src/**",
    "gen/**",
    "**/*.{json,html}",
    ".github/**",
    "web/*",
    "**/vendor/**",
];

/// The order "lowering" refers to: `needs-split` stops the PR, so it ranks highest.
fn rank(tier: Tier) -> u8 {
    match tier {
        Tier::Trivial => 0,
        Tier::Micro => 1,
        Tier::Lite => 2,
        Tier::Full => 3,
        Tier::NeedsSplit => 4,
    }
}

fn path() -> impl Strategy<Value = String> {
    select(PATHS.to_vec()).prop_map(str::to_owned)
}

fn lines() -> impl Strategy<Value = Lines> {
    prop_oneof![
        8 => (0_u64..120, 0_u64..120).prop_map(|(added, removed)| Lines::Counted { added, removed }),
        1 => Just(Lines::Binary),
    ]
}

fn file_change() -> impl Strategy<Value = FileChange> {
    (path(), proptest::option::of(path()), lines()).prop_map(|(path, old, lines)| {
        let change = FileChange::new(path, lines);
        match old {
            Some(old) => change.renamed_from(old),
            None => change,
        }
    })
}

fn globs() -> impl Strategy<Value = Vec<Glob>> {
    vec(select(PATTERNS.to_vec()), 0..3).prop_map(|patterns| {
        patterns
            .into_iter()
            .filter_map(|pattern| pattern.parse().ok())
            .collect()
    })
}

fn repo_globs() -> impl Strategy<Value = RepoGlobs> {
    (globs(), globs(), globs(), globs()).prop_map(|(risk, trust, exclude, docs)| RepoGlobs {
        risk,
        trust,
        exclude,
        docs,
    })
}

proptest! {
    #[test]
    fn adding_a_risk_glob_never_lowers_the_tier(
        files in vec(file_change(), 0..6),
        repo in repo_globs(),
        extra in select(PATTERNS.to_vec()),
    ) {
        let set = changes(files);
        let before = tier(&repo, &set).tier;
        let mut riskier = repo;
        riskier.risk.push(extra.parse().map_err(|e| TestCaseError::fail(format!("{e}")))?);

        let after = tier(&riskier, &set).tier;

        prop_assert!(rank(after) >= rank(before), "{before:?} became {after:?}");
    }

    #[test]
    fn no_repo_glob_moves_a_trust_path_below_full(
        files in vec(file_change(), 0..6),
        repo in repo_globs(),
    ) {
        let mut files = files;
        files.push(FileChange::new(".github/workflows/ci.yml", Lines::Counted { added: 1, removed: 0 }));

        let tiered = tier(&repo, &changes(files)).tier;

        prop_assert!(rank(tiered) >= rank(Tier::Full), "trust path tiered {tiered:?}");
    }

    #[test]
    fn rust_code_is_never_trivial(
        files in vec(file_change(), 0..6),
        repo in repo_globs(),
        added in 0_u64..3,
    ) {
        let mut files = files;
        files.push(FileChange::new("src/main.rs", Lines::Counted { added, removed: 0 }));

        let tiered = tier(&repo, &changes(files)).tier;

        prop_assert_ne!(tiered, Tier::Trivial);
    }

    #[test]
    fn a_rename_tiers_the_same_whichever_side_is_the_head(
        old in path(),
        new in path(),
        lines in lines(),
        repo in repo_globs(),
    ) {
        let forward = tier(&repo, &changes(vec![FileChange::new(new.clone(), lines).renamed_from(old.clone())]));
        let backward = tier(&repo, &changes(vec![FileChange::new(old, lines).renamed_from(new)]));

        prop_assert_eq!(
            (forward.tier, forward.tier_reasons, forward.classes, forward.trust_paths_changed),
            (backward.tier, backward.tier_reasons, backward.classes, backward.trust_paths_changed)
        );
    }
}

/// Paths around the roots of [`ROOTS`], in several ASCII cases, none a build script by name.
const ROOT_PATHS: [&str; 14] = [
    "crates/a",
    "CRATES/A/src/lib.rs",
    "crates/ab/x.rs",
    "crates/b/codegen/gen.rs",
    "crates/B/SRC/lib.rs",
    "crates/b/inner/src/x.rs",
    "crates/b/Inner/gen.RS",
    "crates/b//x.rs",
    "tools/gen.rs",
    "Tools/Gen.rs/x",
    "lib.rs",
    "a/mod.rs",
    "a/b/mod.rs",
    "docs/x.md",
];

/// Exec roots of every kind, canonical or not, nested in each other.
fn root() -> impl Strategy<Value = ExecRoot> {
    let path = |path: &str| ExecRoot::Path(path.to_owned());
    let sources = |dir: &str| ExecRoot::BuildSources {
        crate_dir: dir.to_owned(),
    };
    select(vec![
        path("crates/a"),
        path("crates/A/src"),
        path("tools/GEN.rs"),
        path("crates"),
        path(""),
        path("a/../b"),
        sources("crates/b"),
        sources("Crates/B/inner"),
        sources(""),
        sources("x//y"),
        ExecRoot::Everything,
    ])
}

proptest! {
    #[test]
    fn exec_surface_is_every_side_some_root_holds(
        files in vec(select(ROOT_PATHS.to_vec()), 0..6),
        roots in vec(root(), 0..5),
    ) {
        let mut set = changes(
            files
                .iter()
                .map(|path| FileChange::new(*path, Lines::Counted { added: 1, removed: 0 }))
                .collect(),
        );
        set.exec_roots = roots;
        let expected: Vec<String> = files
            .into_iter()
            .filter(|path| set.exec_roots.iter().any(|root| root.holds(path)))
            .map(str::to_owned)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        let surface = tier(&RepoGlobs::default(), &set).exec_surface_changed;

        prop_assert_eq!(surface, expected);
    }
}
