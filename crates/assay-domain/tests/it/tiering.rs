//! Tiering (`docs/architecture/04-pipeline.md` §2.1 to §2.3, `05-security.md` T17): one test
//! per row of the tier table, per trust-path glob, per risk glob and per filtering rule.

use assay_domain::{
    BaselineChange, ExecCause, FileChange, Lines, ManifestKey, ManifestTrustChange, PackageChange,
    PackageDiff, PrClass, RepoGlobs, RequiredRole, Tier, TierReason, tier,
};

use crate::support::{changes, file, globs};

const FULL_ROLES: [RequiredRole; 3] = [
    RequiredRole::Bugs,
    RequiredRole::Security,
    RequiredRole::Design,
];

const META_ROLES: [RequiredRole; 4] = [
    RequiredRole::Bugs,
    RequiredRole::Security,
    RequiredRole::Design,
    RequiredRole::Meta,
];

/// The tier and reasons of `files` with no repo globs.
fn tier_of(files: Vec<FileChange>) -> (Tier, Vec<TierReason>) {
    let tiering = tier(&RepoGlobs::default(), &changes(files));
    (tiering.tier, tiering.tier_reasons)
}

#[test]
fn one_character_rust_change_is_micro_not_trivial() {
    let tiering = tier(
        &RepoGlobs::default(),
        &changes(vec![file("src/lib.rs", 1, 1)]),
    );

    assert_eq!(
        (tiering.tier, tiering.tier_reasons, tiering.required_roles),
        (
            Tier::Micro,
            vec![TierReason::MicroSize],
            vec![RequiredRole::CombinedMicro]
        )
    );
}

#[test]
fn over_5000_filtered_lines_needs_split() {
    assert_eq!(
        tier_of(vec![file("src/lib.rs", 4_000, 1_001)]),
        (Tier::NeedsSplit, vec![TierReason::SplitSize])
    );
}

#[test]
fn exactly_5000_lines_is_full_not_split() {
    assert_eq!(
        tier_of(vec![file("src/lib.rs", 5_000, 0)]),
        (Tier::Full, vec![TierReason::FullSize])
    );
}

#[test]
fn needs_split_has_no_required_roles() {
    let tiering = tier(
        &RepoGlobs::default(),
        &changes(vec![file("a.rs", 5_001, 0)]),
    );

    assert_eq!(tiering.required_roles, Vec::new());
}

#[test]
fn docs_only_change_is_trivial_with_no_roles() {
    let tiering = tier(
        &RepoGlobs::default(),
        &changes(vec![file("README.md", 3, 1), file("docs/guide.md", 40, 0)]),
    );

    assert_eq!(
        (tiering.tier, tiering.tier_reasons, tiering.required_roles),
        (Tier::Trivial, vec![TierReason::DocsOnly], Vec::new())
    );
}

#[test]
fn empty_change_is_micro_not_trivial() {
    assert_eq!(
        tier_of(Vec::new()),
        (Tier::Micro, vec![TierReason::MicroSize])
    );
}

#[test]
fn over_150_lines_is_full_without_meta() {
    let tiering = tier(
        &RepoGlobs::default(),
        &changes(vec![file("src/lib.rs", 100, 51)]),
    );

    assert_eq!(
        (tiering.tier, tiering.tier_reasons, tiering.required_roles),
        (Tier::Full, vec![TierReason::FullSize], FULL_ROLES.to_vec())
    );
}

#[test]
fn lines_21_to_150_are_lite() {
    let tiers = [21, 150].map(|lines| tier_of(vec![file("src/lib.rs", lines, 0)]));

    assert_eq!(
        tiers,
        [0, 0].map(|_| (Tier::Lite, vec![TierReason::LiteSize]))
    );
}

#[test]
fn lite_requires_the_combined_role() {
    let tiering = tier(
        &RepoGlobs::default(),
        &changes(vec![file("src/lib.rs", 30, 0)]),
    );

    assert_eq!(tiering.required_roles, vec![RequiredRole::Combined]);
}

#[test]
fn up_to_20_lines_outside_the_docs_allowlist_are_micro() {
    assert_eq!(
        tier_of(vec![file("src/lib.rs", 10, 10)]),
        (Tier::Micro, vec![TierReason::MicroSize])
    );
}

#[test]
fn zero_line_snapshot_only_change_is_micro() {
    let tiering = tier(
        &RepoGlobs::default(),
        &changes(vec![file("tests/snapshots/out.snap", 12, 3)]),
    );

    assert_eq!(
        (
            tiering.tier,
            tiering.filtered.lines,
            tiering.filtered.excluded
        ),
        (Tier::Micro, 0, vec!["tests/snapshots/out.snap".to_owned()])
    );
}

#[test]
fn binary_numstat_counts_as_maximum_size_and_forces_full() {
    let binary = FileChange::new("assets/logo.bin", Lines::Binary);

    assert_eq!(
        tier_of(vec![binary]),
        (Tier::Full, vec![TierReason::BinaryFile])
    );
}

#[test]
fn binary_file_adds_no_lines_toward_needs_split() {
    let files = vec![
        FileChange::new("assets/logo.bin", Lines::Binary),
        file("src/lib.rs", 4_990, 0),
    ];

    assert_eq!(
        tier_of(files),
        (
            Tier::Full,
            vec![TierReason::FullSize, TierReason::BinaryFile]
        )
    );
}

#[test]
fn build_script_is_exec_surface_wherever_it_lives() {
    let tierings = ["build.rs", "vendor/openssl-sys/build.rs"].map(|path| {
        let tiering = tier(&RepoGlobs::default(), &changes(vec![file(path, 1, 0)]));
        (tiering.tier, tiering.classes, tiering.exec_surface_changed)
    });

    assert_eq!(
        tierings,
        [
            (
                Tier::Full,
                vec![PrClass::ExecSurface],
                vec!["build.rs".to_owned()]
            ),
            (
                Tier::Full,
                vec![PrClass::ExecSurface],
                vec!["vendor/openssl-sys/build.rs".to_owned()]
            ),
        ]
    );
}

#[test]
fn exec_surface_package_forces_full_with_meta() {
    let mut set = changes(vec![file("Cargo.lock", 4, 2)]);
    set.packages = PackageDiff::Computed(vec![PackageChange::ExecSurface {
        package: "serde_derive".to_owned(),
        cause: ExecCause::ProcMacro,
    }]);

    let tiering = tier(&RepoGlobs::default(), &set);

    assert_eq!(
        (
            tiering.tier,
            tiering.tier_reasons,
            tiering.classes,
            tiering.required_roles,
            tiering.exec_surface_changed,
        ),
        (
            Tier::Full,
            vec![TierReason::ExecSurface],
            vec![PrClass::ExecSurface],
            META_ROLES.to_vec(),
            vec!["serde_derive (proc-macro)".to_owned()],
        )
    );
}

#[test]
fn unavailable_package_diff_is_exec_surface() {
    let mut set = changes(vec![file("README.md", 1, 0)]);
    set.packages = PackageDiff::Unavailable;

    let tiering = tier(&RepoGlobs::default(), &set);

    assert_eq!(
        (tiering.tier, tiering.classes),
        (Tier::Full, vec![PrClass::ExecSurface])
    );
}

#[test]
fn lockfile_dependency_change_is_lite() {
    let mut set = changes(vec![file("Cargo.lock", 6, 6)]);
    set.packages = PackageDiff::Computed(vec![PackageChange::Lockfile {
        package: "itoa".to_owned(),
    }]);

    let tiering = tier(&RepoGlobs::default(), &set);

    assert_eq!(
        (tiering.tier, tiering.tier_reasons),
        (Tier::Lite, vec![TierReason::LockfileDependency])
    );
}

#[test]
fn new_registry_package_is_lite() {
    let mut set = changes(vec![file("Cargo.toml", 1, 0)]);
    set.packages = PackageDiff::Computed(vec![PackageChange::Registry {
        package: "itoa".to_owned(),
    }]);

    let tiering = tier(&RepoGlobs::default(), &set);

    assert_eq!(
        (tiering.tier, tiering.tier_reasons),
        (Tier::Lite, vec![TierReason::RegistryPackage])
    );
}

#[test]
fn vendor_change_is_at_least_lite() {
    assert_eq!(
        tier_of(vec![file("vendor/zlib/inflate.c", 2, 2)]),
        (Tier::Lite, vec![TierReason::Vendor])
    );
}

#[test]
fn every_trust_path_glob_forces_full_with_meta() {
    let paths = [
        ".github/workflows/ci.yml",
        ".github/dependabot.yml",
        ".assay/globs.toml",
        ".claude/settings.json",
        "crates/x/.claude/settings.json",
        "REVIEW.md",
        "crates/x/REVIEW.md",
        "AGENTS.md",
        "crates/x/AGENTS.md",
        "CLAUDE.md",
        "crates/x/CLAUDE.md",
        "CODEOWNERS",
        ".github/CODEOWNERS",
        "docs/CODEOWNERS",
        ".gitattributes",
        "assets/.gitattributes",
        ".githooks/pre-commit",
        "clippy.toml",
        "crates/x/clippy.toml",
        ".clippy.toml",
        "rustfmt.toml",
        ".rustfmt.toml",
        "rust-toolchain",
        "rust-toolchain.toml",
        "deny.toml",
        ".cargo/config.toml",
        "crates/x/.cargo/config.toml",
        "scripts/release.sh",
        "xtask/src/main.rs",
        "Makefile",
        "makefile",
        "GNUmakefile",
        "tools/Makefile",
        "justfile",
        ".justfile",
        "package.json",
        "web/package.json",
        "Dockerfile",
        "Dockerfile.ci",
        "docker/Dockerfile",
    ];

    let wrong: Vec<_> = paths
        .iter()
        .filter(|path| {
            let tiering = tier(&RepoGlobs::default(), &changes(vec![file(path, 1, 0)]));
            (
                tiering.tier,
                tiering.classes,
                tiering.required_roles,
                tiering.trust_paths_changed,
            ) != (
                Tier::Full,
                vec![PrClass::TrustPath],
                META_ROLES.to_vec(),
                vec![(**path).to_owned()],
            )
        })
        .collect();

    assert_eq!(wrong, Vec::<&&str>::new());
}

#[test]
fn trust_path_matching_ignores_ascii_case() {
    let tiering = tier(
        &RepoGlobs::default(),
        &changes(vec![file(".GitHub/Workflows/ci.yml", 1, 0)]),
    );

    assert_eq!(tiering.classes, vec![PrClass::TrustPath]);
}

#[test]
fn near_misses_of_trust_globs_are_not_trust_paths() {
    let paths = [
        "src/github.rs",
        "docs/scripts.md",
        "crates/x/src/makefile_parser.rs",
    ];

    let classes: Vec<_> = paths
        .iter()
        .map(|path| tier(&RepoGlobs::default(), &changes(vec![file(path, 1, 0)])).classes)
        .collect();

    assert_eq!(classes, vec![Vec::<PrClass>::new(); 3]);
}

#[test]
fn every_manifest_trust_key_forces_full() {
    let wrong: Vec<_> = ManifestKey::ALL
        .into_iter()
        .filter(|key| {
            let mut set = changes(vec![file("crates/x/Cargo.toml", 1, 1)]);
            set.manifests = vec![ManifestTrustChange {
                path: "crates/x/Cargo.toml".to_owned(),
                key: *key,
            }];
            let tiering = tier(&RepoGlobs::default(), &set);
            (tiering.tier, tiering.classes, tiering.trust_paths_changed)
                != (
                    Tier::Full,
                    vec![PrClass::TrustPath],
                    vec![format!("crates/x/Cargo.toml ({})", key.code())],
                )
        })
        .collect();

    assert_eq!(wrong, Vec::new());
}

#[test]
fn repo_trust_glob_adds_a_trust_path() -> Result<(), assay_domain::GlobError> {
    let repo = globs(&[], &["crates/**"], &[], &[])?;

    let tiering = tier(&repo, &changes(vec![file("crates/x/src/lib.rs", 1, 0)]));

    assert_eq!(
        (tiering.tier, tiering.classes),
        (Tier::Full, vec![PrClass::TrustPath])
    );
    Ok(())
}

#[test]
fn rename_out_of_auth_stays_full() {
    let moved = file("src/session/login.rs", 1, 1).renamed_from("src/auth/login.rs");

    assert_eq!(
        tier_of(vec![moved]),
        (Tier::Full, vec![TierReason::RiskGlob])
    );
}

#[test]
fn rename_out_of_a_trust_path_names_the_old_path() {
    let moved = file("ci/build.yml", 0, 0).renamed_from(".github/workflows/build.yml");

    let tiering = tier(&RepoGlobs::default(), &changes(vec![moved]));

    assert_eq!(
        (tiering.tier, tiering.trust_paths_changed),
        (Tier::Full, vec![".github/workflows/build.yml".to_owned()])
    );
}

#[test]
fn rename_out_of_docs_is_not_trivial() {
    let moved = file("src/notes.md", 0, 0).renamed_from("docs/notes.md");

    assert_eq!(
        tier_of(vec![moved]),
        (Tier::Micro, vec![TierReason::MicroSize])
    );
}

#[test]
fn every_risk_glob_forces_full_without_a_class() {
    let paths = [
        "src/auth/login.rs",
        "auth/mod.rs",
        "src/Auth/token.rs",
        "src/crypto/aes.rs",
        "db/migrations/001_init.rs",
        "src/unsafe_impl.rs",
        "src/unsafe/mod.rs",
        "queries/report.sql",
    ];

    let wrong: Vec<_> = paths
        .iter()
        .filter(|path| {
            let tiering = tier(&RepoGlobs::default(), &changes(vec![file(path, 1, 0)]));
            (tiering.tier, tiering.tier_reasons, tiering.classes)
                != (Tier::Full, vec![TierReason::RiskGlob], Vec::new())
        })
        .collect();

    assert_eq!(wrong, Vec::<&&str>::new());
}

#[test]
fn repo_risk_glob_forces_full() -> Result<(), assay_domain::GlobError> {
    let repo = globs(&["**/billing/**"], &[], &[], &[])?;

    let tiering = tier(&repo, &changes(vec![file("src/billing/invoice.rs", 1, 0)]));

    assert_eq!(
        (tiering.tier, tiering.tier_reasons),
        (Tier::Full, vec![TierReason::RiskGlob])
    );
    Ok(())
}

#[test]
fn repo_risk_glob_on_a_docs_file_blocks_trivial() -> Result<(), assay_domain::GlobError> {
    let repo = globs(&["docs/security/**"], &[], &[], &[])?;

    let tiering = tier(&repo, &changes(vec![file("docs/security/model.md", 1, 0)]));

    assert_eq!(tiering.tier, Tier::Full);
    Ok(())
}

#[test]
fn every_docs_allowlist_glob_is_trivial() {
    let paths = [
        "README.md",
        "CHANGELOG.md",
        "LICENSE",
        "LICENSE-MIT",
        "docs/guide.md",
        "docs/a/b/notes.txt",
        "docs/img/diagram.png",
        "docs/photo.jpg",
        "docs/arch.svg",
    ];

    let wrong: Vec<_> = paths
        .iter()
        .filter(|path| tier_of(vec![file(path, 2, 1)]).0 != Tier::Trivial)
        .collect();

    assert_eq!(wrong, Vec::<&&str>::new());
}

#[test]
fn files_outside_the_docs_allowlist_are_not_trivial() {
    let paths = [
        "crates/x/README.md",
        "readme.md",
        "docs/page.html",
        "notes.txt",
    ];

    let wrong: Vec<_> = paths
        .iter()
        .filter(|path| tier_of(vec![file(path, 2, 1)]).0 == Tier::Trivial)
        .collect();

    assert_eq!(wrong, Vec::<&&str>::new());
}

#[test]
fn never_trivial_files_stay_out_of_trivial_even_when_a_repo_allows_them()
-> Result<(), assay_domain::GlobError> {
    let repo = globs(&[], &[], &[], &["**"])?;
    let paths = [
        "skills/review/SKILL.md",
        "docs/agents/reviewer.md",
        "docs/commands/run.md",
        "docs/hooks/pre.md",
        ".kb/index.md",
        "REVIEW.md",
        "AGENTS.md",
        "CLAUDE.md",
        "plugin/prompts/v1/system.md",
        "skills/notes.txt",
        "src/lib.rs",
        "build.rs",
        "tools/run.sh",
        "Cargo.toml",
    ];

    let trivial: Vec<_> = paths
        .iter()
        .filter(|path| tier(&repo, &changes(vec![file(path, 1, 0)])).tier == Tier::Trivial)
        .collect();

    assert_eq!(trivial, Vec::<&&str>::new());
    Ok(())
}

#[test]
fn repo_docs_glob_extends_the_allowlist() -> Result<(), assay_domain::GlobError> {
    let repo = globs(&[], &[], &[], &["guides/**/*.md"])?;

    let tiering = tier(&repo, &changes(vec![file("guides/setup.md", 5, 0)]));

    assert_eq!(tiering.tier, Tier::Trivial);
    Ok(())
}

#[test]
fn readme_referenced_by_include_str_is_not_trivial() {
    let mut set = changes(vec![file("README.md", 1, 1)]);
    set.include_targets = vec!["README.md".to_owned()];

    let tiering = tier(&RepoGlobs::default(), &set);

    assert_eq!(
        (tiering.tier, tiering.tier_reasons),
        (Tier::Micro, vec![TierReason::MicroSize])
    );
}

#[test]
fn excluded_files_are_named_and_not_counted() -> Result<(), assay_domain::GlobError> {
    let repo = globs(&[], &[], &["gen/**"], &[])?;
    let files = vec![
        file("Cargo.lock", 300, 200),
        file("web/package-lock.json", 900, 0),
        file("yarn.lock", 10, 0),
        file("pnpm-lock.yaml", 10, 0),
        file("static/app.min.js", 1, 1),
        file("tests/snapshots/x.snap", 40, 0),
        file("gen/schema.json", 2_000, 0),
        file("src/lib.rs", 3, 0),
    ];

    let filtered = tier(&repo, &changes(files)).filtered;

    assert_eq!(
        (filtered.files, filtered.lines, filtered.excluded),
        (
            1,
            3,
            [
                "Cargo.lock",
                "web/package-lock.json",
                "yarn.lock",
                "pnpm-lock.yaml",
                "static/app.min.js",
                "tests/snapshots/x.snap",
                "gen/schema.json",
            ]
            .map(str::to_owned)
            .to_vec()
        )
    );
    Ok(())
}

#[test]
fn code_scripts_manifests_vendor_and_trust_paths_are_never_excluded()
-> Result<(), assay_domain::GlobError> {
    let repo = globs(&[], &[], &["**"], &[])?;
    let paths = [
        "src/lib.rs",
        "gen/out.min.rs",
        "build.rs",
        "vendor/x/build.rs",
        "tools/run.sh",
        "tools/run.bash",
        "tools/gen.py",
        "scripts/x.txt",
        "Cargo.toml",
        "vendor/zlib/inflate.c",
        ".github/workflows/ci.yml",
        ".claude/settings.json",
    ];

    let excluded: Vec<_> = paths
        .iter()
        .filter(|path| {
            !tier(&repo, &changes(vec![file(path, 1, 0)]))
                .filtered
                .excluded
                .is_empty()
        })
        .collect();

    assert_eq!(excluded, Vec::<&&str>::new());
    Ok(())
}

#[test]
fn exclusion_needs_both_sides_of_a_rename() -> Result<(), assay_domain::GlobError> {
    let repo = globs(&[], &[], &["gen/**"], &[])?;
    let moved = file("gen/handler.json", 30, 0).renamed_from("api/handler.json");

    let filtered = tier(&repo, &changes(vec![moved])).filtered;

    assert_eq!((filtered.lines, filtered.excluded), (30, Vec::new()));
    Ok(())
}

#[test]
fn lowered_temper_baseline_is_not_a_trust_path() {
    let mut set = changes(vec![file(".claude/temper-baseline.json", 2, 2)]);
    set.temper_baseline = BaselineChange::LoweredOrUnchanged;

    let tiering = tier(&RepoGlobs::default(), &set);

    assert_eq!((tiering.tier, tiering.classes), (Tier::Micro, Vec::new()));
}

#[test]
fn raised_temper_baseline_is_a_trust_path() {
    let mut set = changes(vec![file(".claude/temper-baseline.json", 2, 2)]);
    set.temper_baseline = BaselineChange::Other;

    let tiering = tier(&RepoGlobs::default(), &set);

    assert_eq!(tiering.classes, vec![PrClass::TrustPath]);
}

#[test]
fn renamed_temper_baseline_is_a_trust_path_even_when_lowered() {
    let moved =
        file(".claude/temper-baseline.json", 0, 0).renamed_from(".claude/old-baseline.json");
    let mut set = changes(vec![moved]);
    set.temper_baseline = BaselineChange::LoweredOrUnchanged;

    let tiering = tier(&RepoGlobs::default(), &set);

    assert_eq!(tiering.classes, vec![PrClass::TrustPath]);
}

#[test]
fn both_classes_are_reported_in_order() {
    let tiering = tier(
        &RepoGlobs::default(),
        &changes(vec![file("build.rs", 1, 0), file("deny.toml", 1, 0)]),
    );

    assert_eq!(
        (tiering.tier_reasons, tiering.classes),
        (
            vec![TierReason::TrustPath, TierReason::ExecSurface],
            vec![PrClass::TrustPath, PrClass::ExecSurface]
        )
    );
}

#[test]
fn trust_paths_are_sorted_and_deduplicated() {
    let files = vec![
        file("deny.toml", 1, 0),
        file("AGENTS.md", 1, 0),
        file("deny.toml", 1, 0),
    ];

    let tiering = tier(&RepoGlobs::default(), &changes(files));

    assert_eq!(
        tiering.trust_paths_changed,
        vec!["AGENTS.md".to_owned(), "deny.toml".to_owned()]
    );
}
