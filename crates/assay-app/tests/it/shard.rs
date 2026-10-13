//! Sharding by dependency cluster (`docs/architecture/04-pipeline.md` §3, issue #98): a pack
//! over 120 KiB is split into at most three shards, each a set of clusters of changed files and
//! their direct importers; the run cap scales by the shard count.

use std::collections::BTreeSet;
use std::error::Error;

use assay_app::{
    Import, MAX_DIFF_BYTES, MAX_IMPORTS, MAX_SHARD_FILES, MAX_SHARDS, Shard, ShardCount,
    ShardError, ShardFile, Sharding, full_run_cap, shard,
};
use assay_domain::{Aeu, RequiredRole};
use proptest::collection::vec;
use proptest::prelude::*;

const FULL: [RequiredRole; 3] = [
    RequiredRole::Bugs,
    RequiredRole::Security,
    RequiredRole::Design,
];

const FULL_META: [RequiredRole; 4] = [
    RequiredRole::Bugs,
    RequiredRole::Security,
    RequiredRole::Design,
    RequiredRole::Meta,
];

fn file(path: &str, pack_bytes: usize) -> ShardFile<'_> {
    ShardFile { path, pack_bytes }
}

/// `by` imports `of`.
fn import<'a>(by: &'a str, of: &'a str) -> Import<'a> {
    Import {
        importer: by,
        imported: of,
    }
}

/// Each shard's changed files and importers, in shard order.
fn layout<'a>(sharding: &Sharding<'a>) -> Vec<(Vec<&'a str>, Vec<&'a str>)> {
    sharding
        .shards()
        .iter()
        .map(|shard| (shard.files().to_vec(), shard.importers().to_vec()))
        .collect()
}

/// A third of the budget: three fill a shard.
const THIRD: usize = MAX_DIFF_BYTES / 3;

#[test]
fn a_pack_exactly_at_the_budget_is_one_shard_of_every_file_in_input_order()
-> Result<(), Box<dyn Error>> {
    let files = [
        file("src/b.rs", THIRD),
        file("src/a.rs", THIRD),
        file("src/c.rs", MAX_DIFF_BYTES - 2 * THIRD),
    ];

    let sharding = shard(&files, &[import("src/lib.rs", "src/a.rs")])?;

    assert_eq!(
        layout(&sharding),
        [(vec!["src/b.rs", "src/a.rs", "src/c.rs"], vec![])]
    );
    assert_eq!(sharding.count(), ShardCount::ONE);
    Ok(())
}

#[test]
fn a_pack_one_byte_over_the_budget_is_sharded_by_cluster() -> Result<(), Box<dyn Error>> {
    let files = [
        file("src/a.rs", MAX_DIFF_BYTES / 2),
        file("src/b.rs", MAX_DIFF_BYTES / 2 + 1),
    ];

    let sharding = shard(&files, &[])?;

    assert_eq!(
        layout(&sharding),
        [(vec!["src/b.rs"], vec![]), (vec!["src/a.rs"], vec![])]
    );
    assert_eq!(sharding.count().get(), 2);
    Ok(())
}

#[test]
fn a_changed_file_shares_its_shard_with_its_direct_importers() -> Result<(), Box<dyn Error>> {
    let files = [file("src/a.rs", MAX_DIFF_BYTES), file("src/b.rs", 10)];
    let imports = [
        import("src/main.rs", "src/b.rs"),
        import("src/lib.rs", "src/b.rs"),
        import("src/x.rs", "src/a.rs"),
    ];

    let sharding = shard(&files, &imports)?;

    assert_eq!(
        layout(&sharding),
        [
            (vec!["src/a.rs"], vec!["src/x.rs"]),
            (vec!["src/b.rs"], vec!["src/lib.rs", "src/main.rs"]),
        ]
    );
    Ok(())
}

#[test]
fn an_unchanged_importer_joins_no_cluster_and_is_listed_with_each_shard_it_imports_from()
-> Result<(), Box<dyn Error>> {
    // `lib.rs` declares every module, so joining through it would make one cluster of a crate.
    let files = [
        file("src/a.rs", THIRD * 2),
        file("src/b.rs", THIRD * 2),
        file("src/c.rs", THIRD * 2),
    ];
    let imports = [
        import("src/lib.rs", "src/a.rs"),
        import("src/lib.rs", "src/c.rs"),
        import("src/lib.rs", "src/c.rs"),
    ];

    let sharding = shard(&files, &imports)?;

    assert_eq!(
        layout(&sharding),
        [
            (vec!["src/a.rs"], vec!["src/lib.rs"]),
            (vec!["src/b.rs"], vec![]),
            (vec!["src/c.rs"], vec!["src/lib.rs"]),
        ]
    );
    Ok(())
}

#[test]
fn an_importer_of_two_files_in_one_shard_is_listed_there_once() -> Result<(), Box<dyn Error>> {
    let files = [
        file("src/a.rs", THIRD),
        file("src/b.rs", THIRD),
        file("src/c.rs", MAX_DIFF_BYTES),
    ];
    let imports = [
        import("src/lib.rs", "src/a.rs"),
        import("src/lib.rs", "src/b.rs"),
    ];

    let sharding = shard(&files, &imports)?;

    assert_eq!(
        layout(&sharding),
        [
            (vec!["src/c.rs"], vec![]),
            (vec!["src/a.rs", "src/b.rs"], vec!["src/lib.rs"]),
        ]
    );
    Ok(())
}

#[test]
fn a_changed_file_importing_another_shares_its_shard() -> Result<(), Box<dyn Error>> {
    let files = [
        file("src/a.rs", THIRD * 2),
        file("src/b.rs", THIRD),
        file("src/c.rs", THIRD),
    ];

    let sharding = shard(&files, &[import("src/c.rs", "src/b.rs")])?;

    assert_eq!(
        layout(&sharding),
        [
            (vec!["src/a.rs"], vec![]),
            (vec!["src/b.rs", "src/c.rs"], vec![]),
        ]
    );
    Ok(())
}

#[test]
fn imports_of_unchanged_files_and_self_imports_join_nothing() -> Result<(), Box<dyn Error>> {
    let files = [file("src/a.rs", MAX_DIFF_BYTES), file("src/b.rs", 10)];
    let imports = [
        import("src/a.rs", "src/util.rs"),
        import("src/b.rs", "src/util.rs"),
        import("src/a.rs", "src/a.rs"),
    ];

    let sharding = shard(&files, &imports)?;

    assert_eq!(
        layout(&sharding),
        [(vec!["src/a.rs"], vec![]), (vec!["src/b.rs"], vec![])]
    );
    Ok(())
}

#[test]
fn clusters_that_exactly_fill_three_shards_are_accepted() -> Result<(), Box<dyn Error>> {
    let files = [
        file("a.rs", MAX_DIFF_BYTES),
        file("b.rs", MAX_DIFF_BYTES),
        file("c.rs", THIRD),
        file("d.rs", MAX_DIFF_BYTES - THIRD),
    ];

    let sharding = shard(&files, &[])?;

    assert_eq!(
        layout(&sharding),
        [
            (vec!["a.rs"], vec![]),
            (vec!["b.rs"], vec![]),
            (vec!["c.rs", "d.rs"], vec![]),
        ]
    );
    let bytes: Vec<usize> = sharding.shards().iter().map(Shard::pack_bytes).collect();
    assert_eq!(bytes, [MAX_DIFF_BYTES; 3]);
    Ok(())
}

#[test]
fn one_byte_more_than_three_shards_hold_is_refused() {
    let files = [
        file("a.rs", MAX_DIFF_BYTES),
        file("b.rs", MAX_DIFF_BYTES),
        file("c.rs", THIRD),
        file("d.rs", MAX_DIFF_BYTES - THIRD),
        file("e.rs", 1),
    ];

    assert_eq!(
        shard(&files, &[]),
        Err(ShardError::Overfull {
            withheld_bytes: 1,
            files: 1
        })
    );
}

#[test]
fn a_cluster_of_exactly_the_budget_is_one_shard() -> Result<(), Box<dyn Error>> {
    let files = [
        file("a.rs", MAX_DIFF_BYTES - 1),
        file("b.rs", 1),
        file("c.rs", 1),
    ];

    let sharding = shard(&files, &[import("b.rs", "a.rs")])?;

    assert_eq!(
        layout(&sharding),
        [(vec!["a.rs", "b.rs"], vec![]), (vec!["c.rs"], vec![])]
    );
    Ok(())
}

#[test]
fn a_cluster_one_byte_over_the_budget_is_refused() {
    let files = [
        file("a.rs", MAX_DIFF_BYTES),
        file("b.rs", 1),
        file("c.rs", 5),
    ];

    assert_eq!(
        shard(&files, &[import("a.rs", "b.rs")]),
        Err(ShardError::Overfull {
            withheld_bytes: MAX_DIFF_BYTES + 1,
            files: 2
        })
    );
}

#[test]
fn equal_clusters_keep_input_order() -> Result<(), Box<dyn Error>> {
    let files = [
        file("z.rs", THIRD * 2),
        file("m.rs", THIRD * 2),
        file("a.rs", THIRD * 2),
    ];

    let sharding = shard(&files, &[])?;

    assert_eq!(
        layout(&sharding),
        [
            (vec!["z.rs"], vec![]),
            (vec!["m.rs"], vec![]),
            (vec!["a.rs"], vec![])
        ]
    );
    Ok(())
}

#[test]
fn a_chain_of_imports_through_the_most_files_is_one_cluster() -> Result<(), Box<dyn Error>> {
    let paths: Vec<String> = (1..MAX_SHARD_FILES).map(|n| format!("f{n}.rs")).collect();
    let mut files = vec![file("big.rs", MAX_DIFF_BYTES)];
    files.extend(paths.iter().map(|path| file(path, 40)));
    let imports: Vec<Import<'_>> = paths
        .iter()
        .zip(paths.iter().skip(1))
        .map(|(by, of)| import(by, of))
        .collect();

    let sharding = shard(&files, &imports)?;

    let [big, chain] = sharding.shards() else {
        return Err(format!("{} shards", sharding.shards().len()).into());
    };
    assert_eq!(big.files(), ["big.rs"]);
    assert_eq!(
        (chain.files().len(), chain.pack_bytes()),
        (MAX_SHARD_FILES - 1, (MAX_SHARD_FILES - 1) * 40)
    );
    Ok(())
}

#[test]
fn no_file_is_refused() {
    assert_eq!(shard(&[], &[]), Err(ShardError::NoFiles));
}

#[test]
fn the_most_files_are_sharded_and_one_more_is_refused() -> Result<(), Box<dyn Error>> {
    let paths: Vec<String> = (0..=MAX_SHARD_FILES).map(|n| format!("f{n}.rs")).collect();
    let files: Vec<ShardFile<'_>> = paths.iter().map(|path| file(path, 100)).collect();
    let (at_cap, _) = files
        .split_at_checked(MAX_SHARD_FILES)
        .ok_or("too few files")?;

    let accepted = shard(at_cap, &[])?;
    let refused = shard(&files, &[]);

    assert_eq!(
        accepted
            .shards()
            .iter()
            .map(|s| s.files().len())
            .sum::<usize>(),
        MAX_SHARD_FILES
    );
    assert_eq!(
        refused,
        Err(ShardError::TooManyFiles {
            files: MAX_SHARD_FILES + 1
        })
    );
    Ok(())
}

#[test]
fn a_repeated_file_is_refused() {
    let files = [file("a.rs", 1), file("b.rs", 1), file("a.rs", 1)];

    assert_eq!(
        shard(&files, &[]),
        Err(ShardError::RepeatedFile {
            path: "a.rs".to_owned()
        })
    );
}

#[test]
fn the_most_imports_are_read_and_one_more_is_refused() -> Result<(), Box<dyn Error>> {
    let files = [file("a.rs", MAX_DIFF_BYTES), file("b.rs", 1)];
    let importers: Vec<String> = (0..=MAX_IMPORTS).map(|n| format!("i{n}.rs")).collect();
    let imports: Vec<Import<'_>> = importers
        .iter()
        .map(|importer| import(importer, "b.rs"))
        .collect();
    let (at_cap, _) = imports
        .split_at_checked(MAX_IMPORTS)
        .ok_or("too few imports")?;

    let accepted = shard(&files, at_cap)?;
    let refused = shard(&files, &imports);

    let importers: Vec<usize> = accepted
        .shards()
        .iter()
        .map(|s| s.importers().len())
        .collect();
    assert_eq!(importers, [0, MAX_IMPORTS]);
    assert_eq!(
        refused,
        Err(ShardError::TooManyImports {
            imports: MAX_IMPORTS + 1
        })
    );
    Ok(())
}

#[test]
fn a_shard_count_is_one_to_three() {
    let counts: Vec<Option<usize>> = (0..=MAX_SHARDS + 1)
        .map(|n| ShardCount::new(n).map(ShardCount::get))
        .collect();

    assert_eq!(counts, [None, Some(1), Some(2), Some(3), None]);
}

#[test]
fn the_full_run_cap_scales_by_the_shard_count() -> Result<(), Box<dyn Error>> {
    let caps = |roles: &[RequiredRole]| -> Result<Vec<Aeu>, String> {
        (1..=MAX_SHARDS)
            .map(|n| {
                let count = ShardCount::new(n).ok_or("no count")?;
                Ok(full_run_cap(roles, count))
            })
            .collect()
    };

    assert_eq!(
        caps(&FULL)?,
        [2_300_000, 4_600_000, 6_900_000].map(Aeu::from_micros)
    );
    assert_eq!(
        caps(&FULL_META)?,
        [2_600_000, 5_200_000, 7_800_000].map(Aeu::from_micros)
    );
    Ok(())
}

#[test]
fn the_run_cap_counts_a_repeated_role_once() {
    let repeated = [
        RequiredRole::Bugs,
        RequiredRole::Bugs,
        RequiredRole::Security,
    ];

    assert_eq!(
        full_run_cap(&repeated, ShardCount::ONE),
        full_run_cap(
            &[RequiredRole::Bugs, RequiredRole::Security],
            ShardCount::ONE
        )
    );
}

/// Files `f0.rs`.. with the given sizes, and imports between indices (an index past the files
/// names an unchanged importer `u<n>.rs`).
fn graph(sizes: &[usize], edges: &[(usize, usize)]) -> (Vec<String>, Vec<(String, String)>) {
    let name = |n: usize| {
        if n < sizes.len() {
            format!("f{n}.rs")
        } else {
            format!("u{n}.rs")
        }
    };
    let edges = edges
        .iter()
        .map(|&(importer, imported)| (name(importer), name(imported % sizes.len().max(1))))
        .collect();
    ((0..sizes.len()).map(name).collect(), edges)
}

/// Every invariant of a sharding of `paths` (sized `sizes`) under `edges`.
fn check(
    sharding: &Sharding<'_>,
    paths: &[String],
    sizes: &[usize],
    edges: &[(String, String)],
) -> Result<(), TestCaseError> {
    let shards = sharding.shards();
    prop_assert!((1..=MAX_SHARDS).contains(&shards.len()));
    prop_assert_eq!(sharding.count().get(), shards.len());
    let mut seen: Vec<&str> = shards
        .iter()
        .flat_map(|s| s.files().iter().copied())
        .collect();
    seen.sort_unstable();
    let mut expected: Vec<&str> = paths.iter().map(String::as_str).collect();
    expected.sort_unstable();
    prop_assert_eq!(seen, expected);
    let summed: usize = shards.iter().map(Shard::pack_bytes).sum();
    prop_assert_eq!(summed, sizes.iter().sum::<usize>());
    if shards.len() > 1 {
        prop_assert!(shards.iter().all(|s| s.pack_bytes() <= MAX_DIFF_BYTES));
    }
    let shard_of = |path: &str| shards.iter().position(|s| s.files().contains(&path));
    for shard in shards {
        let distinct: BTreeSet<&str> = shard.importers().iter().copied().collect();
        prop_assert_eq!(distinct.len(), shard.importers().len());
        prop_assert!(
            shard
                .importers()
                .iter()
                .all(|path| shard_of(path).is_none())
        );
    }
    let sharded = sizes.iter().sum::<usize>() > MAX_DIFF_BYTES;
    for (importer, imported) in edges.iter().filter(|_| sharded) {
        let target = shard_of(imported);
        match shard_of(importer) {
            Some(home) => prop_assert_eq!(Some(home), target),
            None => prop_assert!(
                target
                    .and_then(|index| shards.get(index))
                    .is_some_and(|s| s.importers().contains(&importer.as_str()))
            ),
        }
    }
    Ok(())
}

proptest! {
    #[test]
    fn every_changed_file_is_in_exactly_one_shard_or_the_input_is_refused(
        sizes in vec(0..MAX_DIFF_BYTES / 8, 1..24),
        edges in vec((0..40_usize, 0..40_usize), 0..48),
    ) {
        let (paths, edges) = graph(&sizes, &edges);
        let files: Vec<ShardFile<'_>> = paths
            .iter()
            .zip(&sizes)
            .map(|(path, &bytes)| file(path, bytes))
            .collect();
        let imports: Vec<Import<'_>> = edges
            .iter()
            .filter(|(importer, imported)| importer != imported)
            .map(|(importer, imported)| import(importer, imported))
            .collect();
        let real: Vec<(String, String)> = imports
            .iter()
            .map(|edge| (edge.importer.to_owned(), edge.imported.to_owned()))
            .collect();

        let result = shard(&files, &imports);

        prop_assert_eq!(&result, &shard(&files, &imports));
        match result {
            Ok(sharding) => check(&sharding, &paths, &sizes, &real)?,
            Err(ShardError::Overfull { withheld_bytes, files }) => {
                prop_assert!(withheld_bytes > 0 && files > 0);
            }
            Err(other) => prop_assert!(false, "unexpected refusal {other:?}"),
        }
    }
}
