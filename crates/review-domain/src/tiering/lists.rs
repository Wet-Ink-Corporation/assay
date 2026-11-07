//! The glob lists of 04 §2.1 to §2.3: the shipped defaults, which a repo can only add to.
//!
//! Lists that raise a tier or protect a file (trust, risk, code, never-trivial, vendor, build
//! scripts) match wide: ignoring ASCII case and counting a match on any ancestor directory.
//! Lists that lower one (docs allowlist, exclusion) match exactly. Both directions fail closed.

use super::change::is_canonical;
use super::glob::Glob;

/// Trust paths (04 §2.2). Files tools discover in any directory match in any directory.
const TRUST: [&str; 24] = [
    ".github/**",
    ".assay/**",
    "**/.claude/**",
    "**/REVIEW.md",
    "**/AGENTS.md",
    "**/CLAUDE.md",
    "**/CODEOWNERS",
    "**/.gitattributes",
    "**/.githooks/**",
    "**/clippy.toml",
    "**/.clippy.toml",
    "**/rustfmt.toml",
    "**/.rustfmt.toml",
    "**/rust-toolchain*",
    "**/deny.toml",
    "**/.cargo/**",
    "scripts/**",
    "xtask/**",
    "**/Makefile",
    "**/GNUmakefile",
    "**/justfile",
    "**/.justfile",
    "**/package.json",
    "**/Dockerfile*",
];

/// Base risk globs (04 §2.3).
const RISK: [&str; 5] = [
    "**/auth/**",
    "**/crypto/**",
    "**/migrations/**",
    "**/unsafe*",
    "**/*.sql",
];

/// Code: `.rs`, build scripts, shell and script files, CI and manifests. Never excluded and
/// never `trivial`, wherever they live (04 §2.1).
const CODE: [&str; 16] = [
    "**/*.rs",
    "**/build.rs",
    "**/*.sh",
    "**/*.bash",
    "**/*.zsh",
    "**/*.fish",
    "**/*.ps1",
    "**/*.bat",
    "**/*.cmd",
    "**/*.py",
    "**/Cargo.toml",
    "**/pyproject.toml",
    ".circleci/**",
    ".buildkite/**",
    "**/azure-pipelines.yml",
    "**/.gitlab-ci.yml",
];

/// Executable agent instructions: never `trivial`, even inside the docs allowlist (04 §2.3).
const NEVER_TRIVIAL: [&str; 10] = [
    "**/SKILL.md",
    "**/agents/*.md",
    "**/commands/*.md",
    "**/hooks/**",
    "**/.kb/**",
    "**/REVIEW.md",
    "**/AGENTS.md",
    "**/CLAUDE.md",
    "**/plugin/**",
    "**/skills/**",
];

/// The docs allowlist (04 §2.3).
const DOCS: [&str; 6] = [
    "README.md",
    "CHANGELOG.md",
    "LICENSE*",
    "docs/**/*.md",
    "docs/**/*.txt",
    "docs/**/*.{png,jpg,svg}",
];

/// Excluded from model context and line counts (04 §2.1): lockfiles' text, minified files and
/// snapshots. Binary files are excluded by their numstat.
const EXCLUDED: [&str; 6] = [
    "**/Cargo.lock",
    "**/package-lock.json",
    "**/yarn.lock",
    "**/pnpm-lock.yaml",
    "**/*.min.*",
    "**/*.snap",
];

/// Vendored code: at least `lite`, never excluded (04 §2.2).
const VENDOR: [&str; 1] = ["**/vendor/**"];

/// Build scripts: exec surface wherever they live, `vendor/` included (04 §2.2).
const BUILD_SCRIPT: [&str; 1] = ["**/build.rs"];

/// The temper baseline, whose lowering alone is not a trust-path change (04 §2.2).
pub(super) const TEMPER_BASELINE: &str = ".claude/temper-baseline.json";

/// Globs a repository adds in base `.assay/globs.toml`. They only add to the shipped lists:
/// no addition removes a trust path, a risk glob, a never-excluded file or a never-trivial file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RepoGlobs {
    /// Extra risk globs: matching files are `full`.
    pub risk: Vec<Glob>,
    /// Extra trust paths, temper's `protected` globs included.
    pub trust: Vec<Glob>,
    /// Extra generated-output globs excluded from model context and line counts.
    pub exclude: Vec<Glob>,
    /// Extra docs-allowlist globs.
    pub docs: Vec<Glob>,
}

/// One shipped list, compiled, and the repository's additions to it, borrowed.
#[derive(Debug)]
struct List<'a> {
    shipped: Vec<Glob>,
    added: &'a [Glob],
}

impl<'a> List<'a> {
    fn new(shipped: &[&str], added: &'a [Glob]) -> Self {
        Self {
            shipped: shipped
                .iter()
                .map(|pattern| Glob::builtin(pattern))
                .collect(),
            added,
        }
    }

    fn globs(&self) -> impl Iterator<Item = &Glob> {
        self.shipped.iter().chain(self.added)
    }

    /// Ignoring ASCII case, on the path or any directory above it.
    fn matches_wide(&self, path: &str) -> bool {
        self.globs().any(|glob| glob.matches_wide(path))
    }

    /// Case-sensitively, on the whole path.
    fn matches_exact(&self, path: &str) -> bool {
        self.globs().any(|glob| glob.matches(path))
    }
}

/// The shipped lists joined with a repository's additions.
#[derive(Debug)]
pub(super) struct Lists<'a> {
    trust: List<'a>,
    risk: List<'a>,
    code: List<'a>,
    never_trivial: List<'a>,
    docs: List<'a>,
    excluded: List<'a>,
    vendor: List<'a>,
    build_script: List<'a>,
}

impl<'a> Lists<'a> {
    pub(super) fn new(repo: &'a RepoGlobs) -> Self {
        Self {
            trust: List::new(&TRUST, &repo.trust),
            risk: List::new(&RISK, &repo.risk),
            code: List::new(&CODE, &[]),
            never_trivial: List::new(&NEVER_TRIVIAL, &[]),
            docs: List::new(&DOCS, &repo.docs),
            excluded: List::new(&EXCLUDED, &repo.exclude),
            vendor: List::new(&VENDOR, &[]),
            build_script: List::new(&BUILD_SCRIPT, &[]),
        }
    }

    /// A trust-path glob matches, or the path is not canonical and so may alias one.
    pub(super) fn is_trust(&self, path: &str) -> bool {
        !is_canonical(path) || self.trust.matches_wide(path)
    }

    pub(super) fn is_risk(&self, path: &str) -> bool {
        self.risk.matches_wide(path)
    }

    pub(super) fn is_vendor(&self, path: &str) -> bool {
        self.vendor.matches_wide(path)
    }

    pub(super) fn is_build_script(&self, path: &str) -> bool {
        self.build_script.matches_wide(path)
    }

    /// In the docs allowlist and not code, vendored or an agent instruction.
    pub(super) fn is_docs(&self, path: &str) -> bool {
        self.docs.matches_exact(path)
            && !self.code.matches_wide(path)
            && !self.never_trivial.matches_wide(path)
            && !self.is_vendor(path)
    }

    /// Matches an exclusion glob.
    pub(super) fn is_excludable(&self, path: &str) -> bool {
        self.excluded.matches_exact(path)
    }

    /// Code, a trust path or vendored: never excluded, whatever the exclusion globs say.
    pub(super) fn is_never_excluded(&self, path: &str) -> bool {
        self.code.matches_wide(path) || self.is_trust(path) || self.is_vendor(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shipped_glob_is_valid() {
        let lists: [&[&str]; 8] = [
            &TRUST,
            &RISK,
            &CODE,
            &NEVER_TRIVIAL,
            &DOCS,
            &EXCLUDED,
            &VENDOR,
            &BUILD_SCRIPT,
        ];

        let invalid: Vec<_> = lists
            .iter()
            .flat_map(|list| list.iter())
            .filter(|pattern| pattern.parse::<Glob>().is_err())
            .collect();

        assert_eq!(invalid, Vec::<&&str>::new());
    }
}
