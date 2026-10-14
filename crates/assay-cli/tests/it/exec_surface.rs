//! Exec-surface detection end to end (04 §2.2, #65): fixture PRs built as git repositories and
//! fetched into an `ObjectRepo`, copied manifest-only by forge, read by the real pinned cargo
//! under `ForgeSupervisor`, and diffed by `assay_app::exec_surface`.
//!
//! Registry crates come from a directory source configured in the test's own `CARGO_HOME`, so
//! nothing touches the network.

use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use assay_app::{ExecSurface, PackageSet, Symlink, exec_surface};
use assay_cli::ForgeSupervisor;
use assay_domain::{ExecCause, ExecRoot, PackageChange, PackageDiff};
use assay_forge::git::{CommitSha, GitEnvironment, GitLimits, ObjectRepo, TreeLimits};
use assay_forge::process::GroupKiller;
use assay_gates::{
    CargoEnvironment, CargoError, CargoLimits, CargoSettings, PinnedCargo, PinnedToolchain,
};
use tempfile::TempDir;

use crate::support::write_executable;

/// The files of one crate in the directory registry.
struct Vendored {
    name: &'static str,
    version: &'static str,
    build_script: bool,
}

/// One side of a fixture PR: files written, a lockfile generated unless `keep_lock`, then
/// `after_lock` written (files cargo must never see while generating it).
#[derive(Default)]
struct Side<'a> {
    files: &'a [(&'a str, &'a str)],
    after_lock: &'a [(&'a str, &'a str)],
    /// Symlinks (path, raw target) made after `files`, before the lockfile.
    links: &'a [(&'a str, &'a str)],
    keep_lock: bool,
}

/// A PR fetched into an object-only repository, with the `CARGO_HOME` that serves its
/// registry crates.
struct Pr {
    scratch: TempDir,
    repo: ObjectRepo,
    base: CommitSha,
    head: CommitSha,
}

const MEMBER: &str = "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";

fn workspace(extra: &str) -> String {
    format!("[workspace]\nmembers = [\"crates/*\"]\nresolver = \"2\"\n{extra}")
}

fn pr(registry: &[Vendored], base: &Side<'_>, head: &Side<'_>) -> Result<Pr, Box<dyn Error>> {
    let scratch = tempfile::tempdir()?;
    vendor(scratch.path(), registry)?;
    let work = scratch.path().join("work");
    plain_git(scratch.path(), &["init", "--quiet", "work"])?;
    let base = commit(scratch.path(), &work, base, "base")?;
    let head = commit(scratch.path(), &work, head, "head")?;
    let git_dir = scratch.path().join("objects.git");
    let repo = ObjectRepo::init(
        &git_dir,
        GitEnvironment::from_process_env()?,
        GitLimits::default(),
    )?;
    let destination = git_dir.to_str().ok_or("scratch path is not UTF-8")?;
    plain_git(
        &work,
        &["push", "--quiet", destination, "HEAD:refs/heads/pr"],
    )?;
    Ok(Pr {
        scratch,
        repo,
        base,
        head,
    })
}

/// Writes the directory registry and a `CARGO_HOME` whose config serves crates.io from it.
fn vendor(scratch: &Path, registry: &[Vendored]) -> Result<(), Box<dyn Error>> {
    let vendor = scratch.join("vendor");
    for (index, krate) in (1_u32..).zip(registry) {
        let dir = vendor.join(format!("{}-{}", krate.name, krate.version));
        let manifest = format!(
            "[package]\nname = \"{}\"\nversion = \"{}\"\nedition = \"2021\"\n",
            krate.name, krate.version
        );
        write(&dir, "Cargo.toml", &manifest)?;
        write(&dir, "src/lib.rs", "")?;
        if krate.build_script {
            write(&dir, "build.rs", "fn main() {}\n")?;
        }
        let checksum = format!("{{\"files\":{{}},\"package\":\"{index:064x}\"}}");
        write(&dir, ".cargo-checksum.json", &checksum)?;
    }
    let config = format!(
        "[source.crates-io]\nreplace-with = \"fixture\"\n[source.fixture]\ndirectory = \"{}\"\n",
        vendor.display()
    );
    write(&scratch.join("cargo-home"), "config.toml", &config)
}

fn commit(
    scratch: &Path,
    work: &Path,
    side: &Side<'_>,
    message: &str,
) -> Result<CommitSha, Box<dyn Error>> {
    for (path, contents) in side.files {
        write(work, path, contents)?;
    }
    for (path, target) in side.links {
        let link = work.join(path);
        if let Some(parent) = link.parent() {
            fs::create_dir_all(parent)?;
        }
        std::os::unix::fs::symlink(target, link)?;
    }
    if !side.keep_lock {
        let generated = test_cargo(scratch, work)
            .args(["generate-lockfile", "--offline"])
            .output()?;
        if !generated.status.success() {
            return Err(String::from_utf8_lossy(&generated.stderr)
                .into_owned()
                .into());
        }
    }
    for (path, contents) in side.after_lock {
        write(work, path, contents)?;
    }
    plain_git(work, &["add", "--all"])?;
    plain_git(work, &["commit", "--quiet", "--allow-empty", "-m", message])?;
    Ok(plain_git(work, &["rev-parse", "HEAD"])?.trim().parse()?)
}

/// The cargo that built this test, for fixture setup only (lockfiles).
fn test_cargo(scratch: &Path, work: &Path) -> Command {
    let mut cargo = Command::new(env!("CARGO"));
    cargo
        .current_dir(work)
        .env_clear()
        .envs(CargoEnvironment::from_vars(env::vars_os()).vars().to_vec())
        .env("CARGO_HOME", scratch.join("cargo-home"))
        .env("RUSTC", rustc());
    cargo
}

fn rustc() -> PathBuf {
    Path::new(env!("CARGO")).with_file_name("rustc")
}

fn write(root: &Path, path: &str, contents: &str) -> Result<(), Box<dyn Error>> {
    let path = root.join(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)?;
    Ok(())
}

/// Runs git as a developer would, isolated from the machine's config. Test setup only.
fn plain_git(cwd: &Path, args: &[&str]) -> Result<String, Box<dyn Error>> {
    let mut git = Command::new("git");
    git.env_clear();
    if let Some(path) = env::var_os("PATH") {
        git.env("PATH", path);
    }
    let output = git
        .current_dir(cwd)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

/// The package set of `commit`, the way `gates-static` computes it: a manifest-only copy, the
/// pinned cargo on it, and its metadata read. `None` when cargo fails (fail closed).
fn package_set(
    pr: &Pr,
    commit: &CommitSha,
    name: &str,
) -> Result<Option<PackageSet>, Box<dyn Error>> {
    let copy =
        pr.repo
            .manifest_copy(commit, &pr.scratch.path().join(name), TreeLimits::default())?;
    let toolchain = PinnedToolchain {
        cargo: PathBuf::from(env!("CARGO")),
        rustc: rustc(),
        // The binaries are the toolchain's own, not rustup proxies, so the name only has to
        // be set: a proxy would read it instead of any toolchain file.
        name: "pinned".to_owned(),
    };
    let settings = CargoSettings::new(
        toolchain,
        pr.scratch.path().join("cargo-home"),
        CargoEnvironment::from_vars(env::vars_os()),
        CargoLimits::DEFAULT,
    )?;
    let mut supervisor = ForgeSupervisor::new(GroupKiller::system()?);
    let root = copy.root().to_str().ok_or("copy path is not UTF-8")?;
    let symlinks = copy
        .symlinks()
        .iter()
        .map(|link| Symlink {
            path: link.path.clone(),
            target: link.target.clone(),
        })
        .collect();
    match PinnedCargo::new(&mut supervisor, settings).metadata(copy.root()) {
        Ok(metadata) => Ok(Some(PackageSet::from_metadata(&metadata, root, symlinks)?)),
        Err(CargoError::Failed { .. }) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn detect(pr: &Pr) -> Result<ExecSurface, Box<dyn Error>> {
    let base = package_set(pr, &pr.base, "base-copy")?;
    let head = package_set(pr, &pr.head, "head-copy")?;
    // The manifests' own roots are pinned by `assay-app`'s `exec_roots` tests; these cases
    // exercise the package sets.
    Ok(exec_surface(base.as_ref(), head.as_ref(), &[]))
}

#[test]
#[cfg_attr(miri, ignore = "runs git and cargo; Miri cannot spawn processes")]
fn a_new_build_script_is_exec_surface() -> Result<(), Box<dyn Error>> {
    let root = workspace("");
    let files = [
        ("Cargo.toml", root.as_str()),
        ("crates/x/Cargo.toml", MEMBER),
        ("crates/x/src/lib.rs", ""),
    ];
    let pr = pr(
        &[],
        &Side {
            files: &files,
            ..Side::default()
        },
        &Side {
            files: &[("crates/x/build.rs", "fn main() {}\n")],
            ..Side::default()
        },
    )?;

    let surface = detect(&pr)?;

    assert_eq!(
        surface.packages,
        PackageDiff::Computed(vec![PackageChange::ExecSurface {
            package: "x".to_owned(),
            cause: ExecCause::BuildScript,
        }])
    );
    Ok(())
}

/// A workspace whose member `x` depends on `dependency` from the registry.
fn depending_on(dependency: &str) -> [(&'static str, String); 3] {
    [
        ("Cargo.toml", workspace("")),
        (
            "crates/x/Cargo.toml",
            format!("{MEMBER}\n[dependencies]\n{dependency}\n"),
        ),
        ("crates/x/src/lib.rs", String::new()),
    ]
}

fn borrowed<'a>(files: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    files
        .iter()
        .map(|(path, text)| (*path, text.as_str()))
        .collect()
}

#[test]
#[cfg_attr(miri, ignore = "runs git and cargo; Miri cannot spawn processes")]
fn a_bumped_crate_gaining_a_build_script_is_exec_surface() -> Result<(), Box<dyn Error>> {
    let registry = [
        Vendored {
            name: "foo",
            version: "1.0.0",
            build_script: false,
        },
        Vendored {
            name: "foo",
            version: "1.1.0",
            build_script: true,
        },
    ];
    let (base, head) = (
        depending_on("foo = \"=1.0.0\""),
        depending_on("foo = \"=1.1.0\""),
    );
    let pr = pr(
        &registry,
        &Side {
            files: &borrowed(&base),
            ..Side::default()
        },
        &Side {
            files: &borrowed(&head),
            ..Side::default()
        },
    )?;

    let surface = detect(&pr)?;

    assert_eq!(
        surface.packages,
        PackageDiff::Computed(vec![PackageChange::ExecSurface {
            package: "foo".to_owned(),
            cause: ExecCause::BuildScript,
        }])
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git and cargo; Miri cannot spawn processes")]
fn a_plain_registry_bump_is_not_exec_surface() -> Result<(), Box<dyn Error>> {
    let registry = [
        Vendored {
            name: "bar",
            version: "1.0.0",
            build_script: false,
        },
        Vendored {
            name: "bar",
            version: "2.0.0",
            build_script: false,
        },
    ];
    let (base, head) = (depending_on("bar = \"1\""), depending_on("bar = \"2\""));
    let pr = pr(
        &registry,
        &Side {
            files: &borrowed(&base),
            ..Side::default()
        },
        &Side {
            files: &borrowed(&head),
            ..Side::default()
        },
    )?;

    let surface = detect(&pr)?;

    assert_eq!(
        surface,
        ExecSurface {
            packages: PackageDiff::Computed(vec![PackageChange::Registry {
                package: "bar".to_owned()
            }]),
            exec_roots: Vec::new(),
        },
        "at least lite and the deny gate, but no owner approval"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git and cargo; Miri cannot spawn processes")]
fn a_path_build_dependency_inherited_from_the_workspace_is_an_exec_root()
-> Result<(), Box<dyn Error>> {
    let root = workspace("\n[workspace.dependencies]\nhelpers = { path = \"crates/helpers\" }\n");
    let member = format!("{MEMBER}\n[build-dependencies]\nhelpers = {{ workspace = true }}\n");
    let files = [
        ("Cargo.toml", root.as_str()),
        ("crates/x/Cargo.toml", member.as_str()),
        ("crates/x/src/lib.rs", ""),
        ("crates/x/build.rs", "fn main() {}\n"),
        (
            "crates/helpers/Cargo.toml",
            "[package]\nname = \"helpers\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        ("crates/helpers/src/lib.rs", ""),
    ];
    let pr = pr(
        &[],
        &Side {
            files: &files,
            ..Side::default()
        },
        &Side {
            files: &[("crates/helpers/src/lib.rs", "pub fn edited() {}\n")],
            ..Side::default()
        },
    )?;

    let surface = detect(&pr)?;

    assert_eq!(
        surface,
        ExecSurface {
            packages: PackageDiff::Computed(Vec::new()),
            exec_roots: vec![ExecRoot::Path("crates/helpers".to_owned())],
        },
        "an in-place edit changes no package, so only the root catches it"
    );
    Ok(())
}

/// The member `x`, with a build script and the build-dependency `helpers` at `helpers_path`
/// (relative to `crates/x`).
fn building_with(helpers_path: &str) -> String {
    format!("{MEMBER}\n[build-dependencies]\nhelpers = {{ path = \"{helpers_path}\" }}\n")
}

const HELPERS: &str = "[package]\nname = \"helpers\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";

#[test]
#[cfg_attr(miri, ignore = "runs git and cargo; Miri cannot spawn processes")]
fn a_symlink_leading_out_of_a_build_time_crate_is_everything() -> Result<(), Box<dyn Error>> {
    // #65 review round 3 F1 (wi-b5affa): `crates/helpers/src/lib.rs` is a symlink to
    // `shared/x.rs`, so an edit to `shared/x.rs` alone runs at build time.
    let root = workspace("");
    let member = building_with("../helpers");
    let files = [
        ("Cargo.toml", root.as_str()),
        ("crates/x/Cargo.toml", member.as_str()),
        ("crates/x/src/lib.rs", ""),
        ("crates/x/build.rs", "fn main() {}\n"),
        ("crates/helpers/Cargo.toml", HELPERS),
        ("shared/x.rs", ""),
    ];
    let pr = pr(
        &[],
        &Side {
            files: &files,
            links: &[("crates/helpers/src/lib.rs", "../../../shared/x.rs")],
            ..Side::default()
        },
        &Side {
            files: &[("shared/x.rs", "pub fn edited() {}\n")],
            ..Side::default()
        },
    )?;

    let surface = detect(&pr)?;

    assert_eq!(
        surface,
        ExecSurface {
            packages: PackageDiff::Computed(Vec::new()),
            exec_roots: vec![ExecRoot::Everything],
        },
        "the root cannot hold the symlink's target, so it is refused rather than modelled"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git and cargo; Miri cannot spawn processes")]
fn a_cargo_directory_symlink_leading_out_of_a_build_time_crate_is_everything()
-> Result<(), Box<dyn Error>> {
    // #65 review round 4 F1: the copy never writes `crates/helpers/.cargo/x.rs`, but the
    // build-time root holds it, so where it leads still counts.
    let root = workspace("");
    let member = building_with("../helpers");
    let files = [
        ("Cargo.toml", root.as_str()),
        ("crates/x/Cargo.toml", member.as_str()),
        ("crates/x/src/lib.rs", ""),
        ("crates/x/build.rs", "fn main() {}\n"),
        ("crates/helpers/Cargo.toml", HELPERS),
        ("crates/helpers/src/lib.rs", ""),
        ("shared/x.rs", ""),
    ];
    let pr = pr(
        &[],
        &Side {
            files: &files,
            links: &[("crates/helpers/.cargo/x.rs", "../../../shared/x.rs")],
            ..Side::default()
        },
        &Side {
            files: &[("shared/x.rs", "pub fn edited() {}\n")],
            ..Side::default()
        },
    )?;

    let surface = detect(&pr)?;

    assert_eq!(
        surface,
        ExecSurface {
            packages: PackageDiff::Computed(Vec::new()),
            exec_roots: vec![ExecRoot::Everything],
        }
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git and cargo; Miri cannot spawn processes")]
fn a_library_path_outside_a_build_time_crate_is_everything() -> Result<(), Box<dyn Error>> {
    // #65 review round 3 F2: `[lib] path = "../shared/lib.rs"` on a build-dependency. Real
    // cargo reports `src_path` unnormalised (`<copy>/helpers/../shared/lib.rs`), and a path
    // with a `..` segment is outside the copy, so this fails closed to `Everything` rather
    // than giving `Path("shared")` (the normalised case is pinned in `assay-app`).
    let root = workspace("");
    let member = building_with("../../helpers");
    let helpers = format!("{HELPERS}\n[lib]\npath = \"../shared/lib.rs\"\n");
    let files = [
        ("Cargo.toml", root.as_str()),
        ("crates/x/Cargo.toml", member.as_str()),
        ("crates/x/src/lib.rs", ""),
        ("crates/x/build.rs", "fn main() {}\n"),
        ("helpers/Cargo.toml", helpers.as_str()),
        ("shared/lib.rs", ""),
    ];
    let pr = pr(
        &[],
        &Side {
            files: &files,
            ..Side::default()
        },
        &Side {
            files: &[("shared/lib.rs", "pub fn edited() {}\n")],
            ..Side::default()
        },
    )?;

    let surface = detect(&pr)?;

    assert_eq!(
        surface,
        ExecSurface {
            packages: PackageDiff::Computed(Vec::new()),
            exec_roots: vec![ExecRoot::Everything],
        },
        "the edit to shared/lib.rs is exec surface"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git and cargo; Miri cannot spawn processes")]
fn an_unresolvable_workspace_dependency_fails_closed() -> Result<(), Box<dyn Error>> {
    let root = workspace("");
    let broken = format!("{MEMBER}\n[build-dependencies]\nmissing = {{ workspace = true }}\n");
    let files = [
        ("Cargo.toml", root.as_str()),
        ("crates/x/Cargo.toml", MEMBER),
        ("crates/x/src/lib.rs", ""),
    ];
    let pr = pr(
        &[],
        &Side {
            files: &files,
            ..Side::default()
        },
        &Side {
            files: &[("crates/x/Cargo.toml", broken.as_str())],
            keep_lock: true,
            ..Side::default()
        },
    )?;

    let surface = detect(&pr)?;

    assert_eq!(
        surface,
        ExecSurface {
            packages: PackageDiff::Unavailable,
            exec_roots: vec![ExecRoot::Everything],
        }
    );
    Ok(())
}

/// Proves only that the PR's root `.cargo/config.toml` `rustc` and `rustc-wrapper` never run
/// (#65 review F3). The other files are passengers: cargo here is the toolchain's own binary,
/// not a rustup proxy, so `rust-toolchain.toml` could not select a toolchain either way, and
/// cargo reads no `.cargo` below its working directory. That the copy holds none of them is
/// pinned by forge's `manifest_copy` tests.
#[test]
#[cfg_attr(miri, ignore = "runs git and cargo; Miri cannot spawn processes")]
fn no_pr_configuration_or_toolchain_file_reaches_cargo() -> Result<(), Box<dyn Error>> {
    let outside = tempfile::tempdir()?;
    let marker = outside.path().join("ran");
    let evil = outside.path().join("evil.sh");
    write_executable(
        &evil,
        &format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
    )?;
    let config = format!(
        "[build]\nrustc = \"{0}\"\nrustc-wrapper = \"{0}\"\n",
        evil.display()
    );
    let root = workspace("");
    let files = [
        ("Cargo.toml", root.as_str()),
        ("crates/x/Cargo.toml", MEMBER),
        ("crates/x/src/lib.rs", ""),
    ];
    let pr = pr(
        &[],
        &Side {
            files: &files,
            ..Side::default()
        },
        &Side {
            after_lock: &[
                (".cargo/config.toml", config.as_str()),
                ("crates/x/.cargo/config.toml", config.as_str()),
                (
                    "rust-toolchain.toml",
                    "[toolchain]\nchannel = \"0.0.0-evil\"\n",
                ),
                (".gitattributes", "Cargo.toml -diff\n"),
            ],
            ..Side::default()
        },
    )?;

    let surface = detect(&pr)?;

    assert_eq!(
        (surface.packages, marker.exists()),
        (PackageDiff::Computed(Vec::new()), false),
        "the pinned cargo computed the set, and the PR's wrapper never ran"
    );
    Ok(())
}
