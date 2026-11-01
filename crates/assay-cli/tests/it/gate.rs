//! `assay gate <job>` (#67): the entry point a Z2b job runs. In `gates-test` it runs the base's
//! declared `test` steps and `exec` gates through `run_gates`, writes each concluded gate's
//! document as `<out>/<plugin>.json` and prints one line per gate. `gates-lint` runs its `lint`
//! steps here once (the rest is `gate_lint.rs`'s), and `gates-static` is `gate_static.rs`'s.
//!
//! The steps are `/bin/sh -c` scripts run from the head tree, so no cargo runs here.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use assay_app::{Contract, ContractValidator};
use tempfile::TempDir;

use crate::gate_static_scene::Scene;

const DIGEST: &str = "sha256:dc7c1d8e3ccab68cfdb828ce40a446c3913f7172f899ffe3f439839cfd917a8b";
const PIN: &str = "git:0123456789abcdef0123456789abcdef01234567";
pub(crate) const RUN_KEY: &str = "sha256:gate-test";

/// A temper plugin whose declared steps are `steps`, as `(name, job, shell script)`.
pub(crate) fn temper(steps: &[(&str, &str, &str)]) -> String {
    let head = format!(
        "[[plugin]]\nname = \"temper\"\nadapter = \"builtin:temper\"\npin = \"{PIN}\"\n\
         required = true\n"
    );
    let declared = steps.iter().map(|(name, job, script)| {
        format!(
            "\n[[plugin.step]]\nname = \"{name}\"\njob = \"{job}\"\n\
             cmd = [\"/bin/sh\", \"-c\", {script:?}]\ntimeout_s = 60\n"
        )
    });
    std::iter::once(head).chain(declared).collect()
}

/// A gates job's scratch: a head tree, a base config outside it, an output path not yet
/// there and an empty `GITHUB_OUTPUT` file, as the runner creates it.
pub(crate) struct Job {
    pub(crate) dir: TempDir,
    pub(crate) head: PathBuf,
    pub(crate) config: PathBuf,
    pub(crate) out: PathBuf,
    pub(crate) github_output: PathBuf,
}

impl Job {
    /// A job whose base config holds `plugins` after its `[assay]` table.
    pub(crate) fn new(plugins: &str) -> Result<Self, Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        let head = dir.path().join("head");
        fs::create_dir(&head)?;
        let config = dir.path().join("plugins.toml");
        fs::write(
            &config,
            format!("[assay]\ndigest = \"{DIGEST}\"\n\n{plugins}"),
        )?;
        let out = dir.path().join("out");
        let github_output = dir.path().join("github-output");
        fs::write(&github_output, "")?;
        Ok(Self {
            dir,
            head,
            config,
            out,
            github_output,
        })
    }

    /// `assay gate <job> --config <config> --head <head> --out <out>`.
    pub(crate) fn run(&self, job: &str) -> Result<Output, Box<dyn Error>> {
        self.run_with(job, &self.config, Some(RUN_KEY))
    }

    /// As [`Job::run`], with `config` and `run_key` (unset when `None`).
    pub(crate) fn run_with(
        &self,
        job: &str,
        config: &Path,
        run_key: Option<&str>,
    ) -> Result<Output, Box<dyn Error>> {
        let mut command = self.command(job, config, run_key);
        command.env("GITHUB_OUTPUT", &self.github_output);
        Ok(command.output()?)
    }

    /// The command [`Job::run_with`] runs, without `GITHUB_OUTPUT`.
    pub(crate) fn command(&self, job: &str, config: &Path, run_key: Option<&str>) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_assay"));
        command
            .arg("gate")
            .arg(job)
            .arg("--config")
            .arg(config)
            .arg("--head")
            .arg(&self.head)
            .arg("--out")
            .arg(&self.out)
            .current_dir(self.dir.path())
            .env_clear()
            .env("PATH", "/usr/bin:/bin");
        if let Some(run_key) = run_key {
            command.env("ASSAY_RUN_KEY", run_key);
        }
        command
    }

    /// What the job appended to `GITHUB_OUTPUT`.
    pub(crate) fn outputs(&self) -> Result<String, Box<dyn Error>> {
        Ok(fs::read_to_string(&self.github_output)?)
    }

    /// The names of the files in the output directory, sorted.
    pub(crate) fn written(&self) -> Result<Vec<String>, Box<dyn Error>> {
        let mut names = fs::read_dir(&self.out)?
            .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
            .collect::<Result<Vec<String>, std::io::Error>>()?;
        names.sort();
        Ok(names)
    }

    /// `<out>/<plugin>.json`, validated as `assay.findings/1`: its source name, run key and
    /// conclusion.
    fn document(&self, plugin: &str) -> Result<[String; 3], Box<dyn Error>> {
        let bytes = fs::read(self.out.join(format!("{plugin}.json")))?;
        let document = ContractValidator::new()?.read(Contract::Findings, &bytes)?;
        let field = |pointer: &str| {
            document
                .pointer(pointer)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("{plugin}.json has no {pointer}"))
        };
        Ok([
            field("/source/name")?,
            field("/source/run_key")?,
            field("/conclusion")?,
        ])
    }

    /// Whether `marker` exists in the head tree (a step leaves it to show it ran).
    pub(crate) fn ran(&self, marker: &str) -> bool {
        self.head.join(marker).exists()
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn passing_test_steps_write_a_passing_temper_document() -> Result<(), Box<dyn Error>> {
    let job = Job::new(&temper(&[
        ("fmt", "lint", "touch fmt-ran"),
        ("tests", "test", "touch tests-ran"),
        ("doctests", "test", "touch doctests-ran"),
    ]))?;

    let output = job.run("gates-test")?;

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(stdout(&output), "temper pass\n");
    assert_eq!(job.written()?, ["temper.json"]);
    assert_eq!(job.document("temper")?, ["temper", RUN_KEY, "pass"]);
    assert_eq!(
        [
            job.ran("tests-ran"),
            job.ran("doctests-ran"),
            job.ran("fmt-ran")
        ],
        [true, true, false],
        "both test steps ran from the head tree; the lint step is gates-lint's"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn zero_tests_write_a_failing_document_and_still_exit_0() -> Result<(), Box<dyn Error>> {
    // nextest's exit code when `--no-tests=fail` finds no test to run.
    let job = Job::new(&temper(&[
        ("tests", "test", "exit 4"),
        ("doctests", "test", "touch doctests-ran"),
    ]))?;

    let output = job.run("gates-test")?;

    assert_eq!(
        output.status.code(),
        Some(0),
        "the document speaks: {output:?}"
    );
    assert_eq!(stdout(&output), "temper fail\n");
    assert_eq!(job.document("temper")?, ["temper", RUN_KEY, "fail"]);
    assert!(job.ran("doctests-ran"), "the next step still runs");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_step_a_signal_ended_is_run_once_more() -> Result<(), Box<dyn Error>> {
    let once = "if [ -e first ]; then exit 0; fi; touch first; kill -KILL $$";
    let job = Job::new(&temper(&[("tests", "test", once)]))?;

    let output = job.run("gates-test")?;

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(stdout(&output), "temper pass\n", "the retry decides");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_step_signalled_twice_is_a_gate_error_without_a_document() -> Result<(), Box<dyn Error>> {
    let job = Job::new(&temper(&[("tests", "test", "kill -KILL $$")]))?;

    let output = job.run("gates-test")?;

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(stdout(&output), "temper gate-error:signal\n");
    assert_eq!(
        job.written()?,
        Vec::<String>::new(),
        "a gate error has no document"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn an_exec_gate_of_the_test_job_writes_its_document() -> Result<(), Box<dyn Error>> {
    let document = format!(
        r#"{{"schema":"assay.findings/1","source":{{"kind":"gate","name":"probe","version":"v","run_key":"{RUN_KEY}"}},"conclusion":"neutral","findings":[]}}"#
    );
    let script = format!("printf '%s' '{document}' > \"$ASSAY_FINDINGS_OUT\"");
    let plugins = format!(
        "{}\n[[plugin]]\nname = \"probe\"\nadapter = \"exec\"\n\
         exec = [\"/bin/sh\", \"-c\", {script:?}]\njob = \"test\"\nrequired = false\n\n\
         [[plugin]]\nname = \"lint-probe\"\nadapter = \"exec\"\n\
         exec = [\"/bin/sh\", \"-c\", \"touch lint-probe-ran\"]\njob = \"lint\"\nrequired = false\n",
        temper(&[("tests", "test", "exit 0")])
    );
    let job = Job::new(&plugins)?;

    let output = job.run("gates-test")?;

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(stdout(&output), "temper pass\nprobe neutral\n");
    assert_eq!(job.written()?, ["probe.json", "temper.json"]);
    assert_eq!(job.document("probe")?, ["probe", RUN_KEY, "neutral"]);
    assert!(
        !job.ran("lint-probe-ran"),
        "a lint exec gate is gates-lint's"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn an_existing_out_path_is_refused_before_any_step_runs() -> Result<(), Box<dyn Error>> {
    let job = Job::new(&temper(&[("tests", "test", "touch tests-ran")]))?;
    fs::create_dir(&job.out)?;

    let output = job.run("gates-test")?;

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(!job.ran("tests-ran"), "nothing runs");
    assert_eq!(
        job.written()?,
        Vec::<String>::new(),
        "nothing is written there"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_config_inside_the_head_tree_is_refused() -> Result<(), Box<dyn Error>> {
    let job = Job::new(&temper(&[("tests", "test", "touch tests-ran")]))?;
    let in_head = job.head.join(".assay/plugins.toml");
    fs::create_dir(job.head.join(".assay"))?;
    fs::copy(&job.config, &in_head)?;

    let output = job.run_with("gates-test", &in_head, Some(RUN_KEY))?;

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(!job.ran("tests-ran"));
    assert!(!job.out.exists());
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn the_steps_run_in_the_resolved_head_tree_with_git_capped_above_it() -> Result<(), Box<dyn Error>>
{
    let script = "printf '%s\\n%s' \"$PWD\" \"$GIT_CEILING_DIRECTORIES\" > seen";
    let mut job = Job::new(&temper(&[("tests", "test", script)]))?;
    let resolved = fs::canonicalize(&job.head)?;
    // `head/../head`: the ceiling taken lexically would be `head` itself.
    job.head = job.head.join("../head");

    let output = job.run("gates-test")?;

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let parent = resolved.parent().ok_or("the head tree has a parent")?;
    assert_eq!(
        fs::read_to_string(resolved.join("seen"))?,
        format!("{}\n{}", resolved.display(), parent.display())
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn an_out_path_inside_the_head_tree_is_refused() -> Result<(), Box<dyn Error>> {
    let mut job = Job::new(&temper(&[("tests", "test", "touch tests-ran")]))?;
    job.out = job.head.join("target/gates");
    fs::create_dir(job.head.join("target"))?;

    let output = job.run("gates-test")?;

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(!job.ran("tests-ran"));
    assert!(!job.out.exists());
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_missing_or_empty_run_key_is_refused() -> Result<(), Box<dyn Error>> {
    let job = Job::new(&temper(&[("tests", "test", "touch tests-ran")]))?;

    let codes = [
        job.run_with("gates-test", &job.config, None)?.status.code(),
        job.run_with("gates-test", &job.config, Some(""))?
            .status
            .code(),
    ];

    assert_eq!(codes, [Some(2); 2]);
    assert!(!job.ran("tests-ran"));
    assert!(!job.out.exists());
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn gates_lint_runs_the_lint_steps_and_writes_its_document() -> Result<(), Box<dyn Error>> {
    // Was `gates_lint_is_refused_until_its_inputs_are_wired` (exit 2, `not wired yet`): gates-lint
    // runs since #60 ST-8 (D13, owner-approved) over gate_static_scene's fetched fixture PR.
    // The rest of its behaviour is pinned in `gate_lint.rs`.
    let scene = Scene::lint(
        &[],
        &temper(&[
            ("fmt", "lint", "touch fmt-ran"),
            ("tests", "test", "touch tests-ran"),
        ]),
    )?;

    let output = scene.run(|_| {})?;

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(stdout(&output), "temper pass\n");
    assert_eq!(
        [
            scene.head.join("fmt-ran").exists(),
            scene.head.join("tests-ran").exists()
        ],
        [true, false],
        "the lint step ran from the head tree; the test step is gates-test's"
    );
    let names: Vec<String> = fs::read_dir(scene.path("out"))?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect::<Result<_, std::io::Error>>()?;
    assert_eq!(names, ["temper.json"]);
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn usage_errors_exit_2() -> Result<(), Box<dyn Error>> {
    // The parser's own tests hold the other shapes (repeated, unknown and valueless flags).
    let job = Job::new(&temper(&[("tests", "test", "touch tests-ran")]))?;
    let config = job.config.to_str().ok_or("a UTF-8 path")?;
    let head = job.head.to_str().ok_or("a UTF-8 path")?;
    let out = job.out.to_str().ok_or("a UTF-8 path")?;
    let flags = ["--config", config, "--head", head, "--out", out];
    let cases: [Vec<&str>; 3] = [
        vec!["gate"],
        [&["gate", "gates-nope"][..], &flags].concat(),
        [&["gate", "gates-test"][..], &flags[2..]].concat(),
    ];

    for args in cases {
        let status = Command::new(env!("CARGO_BIN_EXE_assay"))
            .args(&args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("ASSAY_RUN_KEY", RUN_KEY)
            .output()?;

        assert_eq!(status.status.code(), Some(2), "{args:?}");
    }
    assert!(!job.ran("tests-ran"));
    assert!(!job.out.exists());
    Ok(())
}
