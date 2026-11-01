//! The scene `assay gate gates-static`'s (#60 ST-7) and `gates-lint`'s (#60 ST-8) binary tests
//! run in: a fixture PR built as a git repository whose worktree, at the head commit, is the
//! head checkout, the pinned temper copy, a base config outside the head tree, a stand-in `node`
//! and a stand-in `git`, all in one resolved scratch directory.
//!
//! The stand-in `git` (first on `PATH`) records every call and passes it to the real git, except
//! that a fetch from `https://github.com/<REPO>.git` is served from the fixture repository over
//! the file protocol; it records each fetch's argv and the `extraHeader` git was given first, so
//! a test sees what the binary sent without any network. The stand-in `node` records its
//! environment and arguments, and exits 2 unless invoked from the head tree as the pinned scan
//! (it prints an empty scan) or as the pinned `baseline --check` against the base (it writes a
//! clean findings document whose ratchet base is the base, the merge base here).

use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

use crate::support::write_executable;

pub(crate) const REPO: &str = "Wet-Ink-Corporation/assay";
pub(crate) const RUN_KEY: &str = "sha256:gate-static";
pub(crate) const RUN_TOKEN: &str = "ghs_runtoken";
/// `Authorization: basic ` and the base64 of `x-access-token:ghs_runtoken`.
pub(crate) const RUN_TOKEN_HEADER: &str =
    "Authorization: basic eC1hY2Nlc3MtdG9rZW46Z2hzX3J1bnRva2Vu";

pub(crate) const BASELINE: &str = ".claude/temper-baseline.json";
const TEMPER_JSON: &str = "{\n  \"skillGate\": true,\n  \"gate\": {\n    \"scan\": true,\n    \
                           \"stepTimeoutSec\": 900\n  },\n  \"scanner\": {\n    \"exempt\": []\n  }\n}\n";
/// The pinned temper copy, and the same files vendored in the fixture repository.
const TEMPER: [(&str, &str); 3] = [
    ("VERSION", "1.3.0\n"),
    ("lib/scan.mjs", "export function scanSource() {}\n"),
    ("temper.mjs", "#!/usr/bin/env node\n"),
];
const PIN: &str = "oci:sha256:dc7c1d8e3ccab68cfdb828ce40a446c3913f7172f899ffe3f439839cfd917a8b";

/// The baseline with `count` `clippy::too_many_lines` entries for the fixture crate.
pub(crate) fn baseline(count: u64) -> String {
    format!(
        "{{\n  \"temper_baseline\": 1,\n  \"crates\": {{\n    \"fixture\": \".\"\n  }},\n  \
         \"totals\": {{\n    \"fixture\": {{\n      \"clippy::too_many_lines\": {count}\n    }}\n  }}\n}}\n"
    )
}

/// gates-static's base config: the temper and Weigh-In plugins.
fn static_plugins() -> String {
    format!(
        "[[plugin]]\nname = \"temper\"\nadapter = \"builtin:temper\"\npin = \"{PIN}\"\n\
         required = true\n\n[[plugin]]\nname = \"weigh-in\"\nadapter = \"builtin:weigh-in\"\n\
         pin = \"{PIN}\"\nrequired = false\n"
    )
}

/// `(path, text)` files or `(path, target)` symlinks the head commits.
type Pairs<'a> = &'a [(&'a str, &'a str)];

/// A gates-static or gates-lint job's scratch, with the fixture PR's commits.
pub(crate) struct Scene {
    _dir: TempDir,
    /// `gates-static` or `gates-lint`.
    job: &'static str,
    pub(crate) root: PathBuf,
    /// The head checkout, `--head`: the fixture repository, at the head commit.
    pub(crate) head: PathBuf,
    pub(crate) base_sha: String,
    pub(crate) head_sha: String,
}

impl Scene {
    /// A gates-static job over a PR whose head writes `head` over a clean base.
    pub(crate) fn new(head: &[(&str, &str)]) -> Result<Self, Box<dyn Error>> {
        Self::staged("gates-static", (head, &[]), &static_plugins())
    }

    /// A gates-lint job over a PR whose head writes `head` over a clean base, with `plugins`
    /// after base config's `[assay]` table. The base is the merge base (one line of history).
    pub(crate) fn lint(head: &[(&str, &str)], plugins: &str) -> Result<Self, Box<dyn Error>> {
        Self::staged("gates-lint", (head, &[]), plugins)
    }

    /// As [`Scene::lint`], the head also committing each `(path, target)` of `links` as a
    /// symlink.
    pub(crate) fn lint_with_links(
        head: &[(&str, &str)],
        links: &[(&str, &str)],
        plugins: &str,
    ) -> Result<Self, Box<dyn Error>> {
        Self::staged("gates-lint", (head, links), plugins)
    }

    fn staged(
        job: &'static str,
        (head, links): (Pairs<'_>, Pairs<'_>),
        plugins: &str,
    ) -> Result<Self, Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        let root = fs::canonicalize(dir.path())?;
        let work = root.join("work");
        plain_git(&root, &["init", "--quiet", "work"])?;
        write(
            &work,
            "Cargo.toml",
            "[package]\nname = \"fixture\"\n\n[lints]\nworkspace = true\n",
        )?;
        write(&work, "src/lib.rs", "pub fn one() -> u8 {\n    1\n}\n")?;
        write(&work, BASELINE, &baseline(2))?;
        write(&work, ".claude/temper.json", TEMPER_JSON)?;
        for (path, text) in TEMPER {
            write(&work, &format!(".claude/temper/{path}"), text)?;
            write(&root.join("pinned"), path, text)?;
        }
        let base_sha = commit(&work, "base")?;
        for (path, text) in head {
            write(&work, path, text)?;
        }
        for (path, target) in links {
            std::os::unix::fs::symlink(target, work.join(path))?;
        }
        let head_sha = commit(&work, "head")?;
        fs::write(
            root.join("plugins.toml"),
            format!(
                "[assay]\ndigest = \"sha256:{}\"\n\n{plugins}",
                "dc7c1d8e3ccab68cfdb828ce40a446c3913f7172f899ffe3f439839cfd917a8b"
            ),
        )?;
        fs::write(root.join("output"), "")?;
        stand_in_node(&root, &work, &base_sha)?;
        stand_in_git(&root, &work)?;
        Ok(Self {
            _dir: dir,
            job,
            root,
            head: work,
            base_sha,
            head_sha,
        })
    }

    /// `name` under the scene's root.
    pub(crate) fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// Runs git in the head checkout, as a workflow step before the job would. Test setup only.
    pub(crate) fn in_checkout(&self, args: &[&str]) -> Result<String, Box<dyn Error>> {
        plain_git(&self.head, args)
    }

    /// The job's arguments as the workflow passes them, as `(flag, value)` pairs after
    /// `gate <job>`.
    pub(crate) fn flags(&self) -> Vec<(&'static str, PathBuf)> {
        vec![
            ("--config", self.path("plugins.toml")),
            ("--head", self.head.clone()),
            ("--out", self.path("out")),
            ("--node", self.path("bin/node")),
            ("--temper", self.path("pinned")),
        ]
    }

    /// The job with `flags`, every variable set; `ASSAY_MERGE_BASE` (plan's `merge_base`) is the
    /// base, which gates-static does not read.
    pub(crate) fn command(&self, flags: &[(&str, PathBuf)]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_assay"));
        command.args(["gate", self.job]);
        for (flag, value) in flags {
            command.arg(flag).arg(value);
        }
        command
            .current_dir(&self.root)
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.path("git-bin").display()),
            )
            .env("ASSAY_RUN_KEY", RUN_KEY)
            .env("ASSAY_BASE_SHA", &self.base_sha)
            .env("ASSAY_HEAD_SHA", &self.head_sha)
            .env("ASSAY_MERGE_BASE", &self.base_sha)
            .env("ASSAY_RUN_TOKEN", RUN_TOKEN)
            .env("GITHUB_REPOSITORY", REPO)
            .env("GITHUB_OUTPUT", self.path("output"));
        command
    }

    /// Runs the job as the workflow does, after `change` adjusted its variables.
    pub(crate) fn run(&self, change: impl FnOnce(&mut Command)) -> Result<Output, Box<dyn Error>> {
        let mut command = self.command(&self.flags());
        change(&mut command);
        Ok(command.output()?)
    }

    /// Runs the job with `flags`, every variable set.
    pub(crate) fn run_flags(&self, flags: &[(&str, PathBuf)]) -> Result<Output, Box<dyn Error>> {
        Ok(self.command(flags).output()?)
    }

    /// What a stand-in recorded in `name` (`git-bin/calls`, `git-bin/fetches`,
    /// `git-bin/headers`, `bin/node-env`), empty when it never ran.
    pub(crate) fn recorded(&self, name: &str) -> Result<String, Box<dyn Error>> {
        let file = self.path(name);
        Ok(if file.exists() {
            fs::read_to_string(file)?
        } else {
            String::new()
        })
    }

    /// Whether nothing ran and nothing was written: no git, no node, no `--out`, no output.
    pub(crate) fn untouched(&self) -> Result<bool, Box<dyn Error>> {
        Ok(self.recorded("git-bin/calls")?.is_empty()
            && self.recorded("bin/node-env")?.is_empty()
            && !self.path("out").exists()
            && self.recorded("output")?.is_empty())
    }
}

/// What the stand-in `baseline --check` writes: no finding, and a clean `diff+totals` ratchet
/// whose base is `merge_base`, as temper 1.3.0 writes it.
fn findings(merge_base: &str) -> String {
    format!(
        "{{\"temper_findings\":1,\"at\":\"2026-11-01T00:00:00.000Z\",\"commit\":null,\
         \"toolchain\":\"1.98.1\",\"host\":\"x86_64-unknown-linux-gnu\",\
         \"sources\":[\"clippy\",\"rustdoc\",\"scan\"],\"workspace_root\":\".\",\
         \"crates\":[{{\"name\":\"fixture\",\"dir\":\".\",\"lints_inherited\":true,\"charter\":false}}],\
         \"findings\":[],\"errors\":[],\"ratchet\":{{\"ok\":true,\"mode\":\"diff+totals\",\
         \"base\":\"{merge_base}\",\"new_code\":[],\"increases\":[],\"fixed\":[],\"fixed_count\":0,\
         \"legacy_total\":0,\"baseline_total\":0,\"raised\":[],\"errors\":[],\"notes\":[]}}}}"
    )
}

/// Writes `bin/node`: it records its environment in `bin/node-env` and its arguments in
/// `bin/node-args`, and exits 2 unless run from `head` as the pinned scan (printing an empty
/// scan) or as gates-lint's `baseline --check` against `base` (writing [`findings`] with `base`
/// as the merge base).
fn stand_in_node(root: &Path, head: &Path, base: &str) -> Result<(), Box<dyn Error>> {
    let script = root.join("pinned/temper.mjs");
    let body = format!(
        "#!/bin/sh\n/usr/bin/env > \"${{0%/*}}/node-env\"\n\
         printf '%s\\n' \"$*\" >> \"${{0%/*}}/node-args\"\n\
         [ \"$(pwd)\" = '{head}' ] && [ \"$1\" = '{script}' ] || exit 2\n\
         if [ \"$#\" = 4 ] && [ \"$2\" = scan ] && [ \"$3\" = --all ] && [ \"$4\" = --json ]; then\n\
         printf '[]'\nexit 0\nfi\n\
         [ \"$#\" = 8 ] && [ \"$2\" = baseline ] && [ \"$3\" = --check ] && [ \"$4\" = --locked ] \
         && [ \"$5\" = --base ] && [ \"$6\" = '{base}' ] && [ \"$7\" = --findings-out ] \
         && [ \"$8\" = target/temper/findings.json ] || exit 2\n\
         mkdir -p target/temper\nprintf '%s' '{findings}' > target/temper/findings.json\n",
        script = script.display(),
        head = head.display(),
        findings = findings(base),
    );
    fs::create_dir_all(root.join("bin"))?;
    write_executable(&root.join("bin/node"), &body)?;
    Ok(())
}

/// Writes `git-bin/git`, the stand-in described in the module docs, over the real git.
fn stand_in_git(root: &Path, fixture: &Path) -> Result<(), Box<dyn Error>> {
    let real = env::split_paths(&env::var_os("PATH").ok_or("PATH is not set")?)
        .map(|dir| dir.join("git"))
        .find(|git| git.is_file())
        .ok_or("no git on PATH")?;
    let body = format!(
        "#!/bin/sh\ndir=${{0%/*}}\nprintf '%s\\n' \"$*\" >> \"$dir/calls\"\n\
         case \" $* \" in\n*' fetch '*)\n\
         {{ printf fetch; for arg; do printf ' %s' \"$arg\"; done; printf '\\n'; }} >> \"$dir/fetches\"\n\
         printf '%s=%s\\n' \"$GIT_CONFIG_KEY_0\" \"$GIT_CONFIG_VALUE_0\" >> \"$dir/headers\"\n\
         n=$#\nfor arg; do\n  case $arg in\n\
         'https://github.com/{REPO}.git') arg='{fixture}' ;;\n\
         fetch) set -- \"$@\" -c protocol.file.allow=always ;;\n  esac\n\
         set -- \"$@\" \"$arg\"\ndone\nshift \"$n\" ;;\nesac\nexec '{real}' \"$@\"\n",
        fixture = fixture.display(),
        real = real.display(),
    );
    fs::create_dir_all(root.join("git-bin"))?;
    write_executable(&root.join("git-bin/git"), &body)?;
    Ok(())
}

fn write(root: &Path, path: &str, text: &str) -> Result<(), Box<dyn Error>> {
    let path = root.join(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, text)?;
    Ok(())
}

fn commit(work: &Path, message: &str) -> Result<String, Box<dyn Error>> {
    plain_git(work, &["add", "--all"])?;
    plain_git(work, &["commit", "--quiet", "--allow-empty", "-m", message])?;
    Ok(plain_git(work, &["rev-parse", "HEAD"])?.trim().to_owned())
}

/// Runs git the way a developer would, isolated from the machine's config. Test setup only.
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
