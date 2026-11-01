//! `assay release --config <plugins-toml>` (#60 ST-10, S10) end to end: the binary with a
//! cleared environment, the ledger repository's files answered by a stand-in `gh` that keeps
//! them in a file and records every call (no network), and the slot's deletion pushed through a
//! stand-in `git` that reports it and runs the real git for everything else.

use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::SystemTime;

use assay_app::{LedgerPath, LedgerTime};
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::support::write_executable;

const REPO: &str = "Wet-Ink-Corporation/assay";
const REPO_ID: &str = "840213377";
const LEDGER: &str = "Wet-Ink-Corporation/assay-ledger";
const RUN_ID: &str = "39168909542";
const LEASE: &str = "cccccccccccccccccccccccccccccccccccccccc";
const DIGEST: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
const LEDGER_TOKEN: &str = "ghs_ledger_secret_value";
const CONFIG: &str = "base/.assay/plugins.toml";

/// The stand-in `gh`: the ledger repository's contents API over the files in `files.json`
/// beside it, each call appended to `calls`.
const GH: &str = r#"
import base64, hashlib, json, os, re, sys

here = os.path.dirname(os.path.abspath(sys.argv[0]))
args = sys.argv[1:]
method = args[args.index("--method") + 1]
path = args[-1]
body = json.loads(sys.stdin.read() or "null") if "--input" in args else None
with open(os.path.join(here, "calls"), "a") as calls:
    calls.write(method + " " + path + "\n")
files_path = os.path.join(here, "files.json")
files = json.load(open(files_path))
ledger = "Wet-Ink-Corporation/assay-ledger"

def answer(status, value):
    reason = {200: "OK", 201: "Created", 404: "Not Found"}[status]
    json.dump(files, open(files_path, "w"))
    sys.stdout.write("HTTP/2.0 %d %s\n\r\n%s" % (status, reason, json.dumps(value)))
    sys.exit(0 if status < 400 else 1)

missing = {"message": "Not Found"}
if method == "GET" and path == "repos/" + ledger:
    answer(200, {"full_name": ledger})
m = re.fullmatch(r"repos/%s/contents/(.+)" % re.escape(ledger), path)
if m and method == "GET":
    name = m.group(1).replace("%2F", "/")
    if name not in files:
        answer(404, missing)
    raw = files[name].encode()
    sha = hashlib.sha1(raw).hexdigest()
    answer(200, {"type": "file", "encoding": "base64", "size": len(raw), "path": name, "content": base64.b64encode(raw).decode(), "sha": sha})
if m and method == "PUT":
    files[m.group(1).replace("%2F", "/")] = base64.b64decode(body["content"]).decode()
    answer(201, {})
answer(404, missing)
"#;

/// The stand-in `git`: a push reports its ref deleted, as `--porcelain` does, and records its
/// arguments; anything else is the real git.
fn git_stand_in(real: &Path) -> String {
    format!(
        "#!/bin/sh\nfor arg; do if [ \"$arg\" = push ]; then pushed=1; fi; done\n\
         if [ -n \"$pushed\" ]; then\n  for last; do :; done\n  \
         printf '%s\\n' \"$*\" >> \"${{0%/*}}/pushes\"\n  \
         printf 'To https://github.com/{LEDGER}.git\\n-\\t%s\\t[deleted]\\nDone\\n' \"$last\"\n  \
         exit 0\nfi\nexec '{}' \"$@\"\n",
        real.display()
    )
}

/// The first `name` on `PATH`.
fn on_path(name: &str) -> Result<PathBuf, Box<dyn Error>> {
    env::split_paths(&env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| format!("no {name} on PATH").into())
}

/// A run of `release` in a scratch directory with base config, `gh` and `git` stand-ins.
struct Scene {
    root: TempDir,
}

impl Scene {
    /// Base config whose `[assay]` table ends with `extra`, and a ledger holding `files`.
    fn new(extra: &str, files: &Value) -> Result<Self, Box<dyn Error>> {
        let root = tempfile::tempdir()?;
        let bin = root.path().join("bin");
        fs::create_dir_all(&bin)?;
        fs::create_dir_all(root.path().join("base/.assay"))?;
        fs::create_dir_all(root.path().join("home"))?;
        fs::write(
            root.path().join(CONFIG),
            format!("[assay]\ndigest = \"{DIGEST}\"\n{extra}"),
        )?;
        let python = on_path("python3")?;
        write_executable(&bin.join("gh"), &format!("#!{} -I\n{GH}", python.display()))?;
        write_executable(&bin.join("git"), &git_stand_in(&on_path("git")?))?;
        fs::write(bin.join("files.json"), files.to_string())?;
        Ok(Self { root })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// `assay release` with `args`, only `variables`, and `PATH` finding the stand-ins.
    fn release(&self, args: &[&str], variables: &[(&str, &str)]) -> Result<Output, Box<dyn Error>> {
        let mut path = vec![self.path("bin")];
        path.extend(env::split_paths(&env::var_os("PATH").unwrap_or_default()));
        Ok(Command::new(env!("CARGO_BIN_EXE_assay"))
            .env_clear()
            .current_dir(self.root.path())
            .arg("release")
            .args(args)
            .env("PATH", env::join_paths(path)?)
            .env("HOME", self.path("home"))
            .envs(variables.iter().copied())
            .output()?)
    }

    /// The lines the stand-in `name` recorded.
    fn recorded(&self, name: &str) -> Result<Vec<String>, Box<dyn Error>> {
        let file = self.path(&format!("bin/{name}"));
        if !file.exists() {
            return Ok(Vec::new());
        }
        Ok(fs::read_to_string(file)?
            .lines()
            .map(str::to_owned)
            .collect())
    }

    /// The ledger's files after the run.
    fn files(&self) -> Result<Value, Box<dyn Error>> {
        Ok(serde_json::from_str(&fs::read_to_string(
            self.path("bin/files.json"),
        )?)?)
    }
}

/// `toJSON(needs)` for an `admit` that wrote `admit` and a review that ended with `review`
/// after writing `outputs`.
fn needs(admit: &Value, review: &str, outputs: &Value) -> String {
    json!({
        "admit": {"result": "success", "outputs": admit},
        "review": {"result": review, "outputs": outputs},
    })
    .to_string()
}

/// An `admit` holding slot 1 at [`LEASE`].
fn holding() -> Value {
    json!({"admission": "spend", "slot": "1", "lease_commit": LEASE, "tier": "lite"})
}

/// The placeholder review's outputs.
fn placeholder() -> Value {
    json!({"stage": "not-run", "cost_aeu": "0", "documents": "[]"})
}

/// The run variables with `attempt`, `needs` and, unless `None`, the ledger token.
fn variables<'a>(
    attempt: &'a str,
    needs: &'a str,
    token: Option<&'a str>,
) -> Vec<(&'a str, &'a str)> {
    let mut variables = vec![
        ("GITHUB_RUN_ID", RUN_ID),
        ("GITHUB_RUN_ATTEMPT", attempt),
        ("GITHUB_REPOSITORY_ID", REPO_ID),
        ("GITHUB_REPOSITORY", REPO),
        ("ASSAY_NEEDS", needs),
    ];
    if let Some(token) = token {
        variables.push(("ASSAY_LEDGER_TOKEN", token));
    }
    variables
}

/// The `reserved` record admit wrote for this run at `at`, as a ledger line.
fn reserved_line(at: &str) -> String {
    format!(
        concat!(
            r#"{{"schema":"assay.ledger-cost/1","run_uid":{{"repo_id":{repo_id},"run_id":{run_id},"#,
            r#""run_attempt":1}},"run_key":"sha256:{key}","repo":"{repo}","pr":7,"#,
            r#""head_sha":"{head}","tier":"lite","roles":[{{"role":"combined","aeu":"0.74"}}],"#,
            r#""aeu":"0.74","kind":"reserved","cancelled":false,"at":"{at}"}}"#,
            "\n"
        ),
        repo_id = REPO_ID,
        run_id = RUN_ID,
        key = "a".repeat(64),
        repo = REPO,
        head = "b".repeat(40),
        at = at,
    )
}

const LEDGER_CONFIG: &str = "ledger_repo = \"Wet-Ink-Corporation/assay-ledger\"\n";

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn an_admission_without_a_slot_ends_successfully_without_any_port() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("", &json!({}))?;
    let needs = needs(&json!({"admission": "no-spend"}), "skipped", &json!({}));

    let ran = scene.release(&["--config", "missing.toml"], &variables("1", &needs, None))?;

    assert_eq!(ran.status.code(), Some(0), "{ran:?}");
    assert!(
        String::from_utf8(ran.stdout)?.contains("no slot"),
        "the line says why nothing was done"
    );
    assert_eq!(scene.recorded("calls")?, Vec::<String>::new());
    assert_eq!(scene.recorded("pushes")?, Vec::<String>::new());
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_re_run_is_refused_before_the_token_is_read() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new(LEDGER_CONFIG, &json!({}))?;
    let needs = needs(&holding(), "success", &placeholder());

    let ran = scene.release(
        &["--config", CONFIG],
        &variables("2", &needs, Some("not\u{7f}a token")),
    )?;

    let stderr = String::from_utf8(ran.stderr)?;
    assert_eq!(ran.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("attempt 2"), "{stderr}");
    assert!(!stderr.contains("ASSAY_LEDGER_TOKEN"), "{stderr}");
    assert_eq!(scene.recorded("calls")?, Vec::<String>::new());
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_held_slot_without_the_ledger_token_cannot_run() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new(LEDGER_CONFIG, &json!({}))?;
    let needs = needs(&holding(), "success", &placeholder());

    let ran = scene.release(&["--config", CONFIG], &variables("1", &needs, None))?;

    let stderr = String::from_utf8(ran.stderr)?;
    assert_eq!(ran.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("ASSAY_LEDGER_TOKEN is not set"), "{stderr}");
    assert_eq!(scene.recorded("calls")?, Vec::<String>::new());
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_config_without_a_ledger_repo_cannot_run() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("", &json!({}))?;
    let needs = needs(&holding(), "success", &placeholder());

    let ran = scene.release(
        &["--config", CONFIG],
        &variables("1", &needs, Some(LEDGER_TOKEN)),
    )?;

    let stderr = String::from_utf8(ran.stderr)?;
    assert_eq!(ran.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("[assay].ledger_repo"), "{stderr}");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn any_other_arguments_are_a_usage_error() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new(LEDGER_CONFIG, &json!({}))?;
    let needs = needs(&holding(), "success", &placeholder());
    for args in [
        &[][..],
        &["--config"],
        &["--config", CONFIG, "extra"],
        &["--out", CONFIG],
    ] {
        let ran = scene.release(args, &variables("1", &needs, Some(LEDGER_TOKEN)))?;

        let stderr = String::from_utf8(ran.stderr)?;
        assert_eq!(ran.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(
            stderr.contains("usage: assay release --config"),
            "{args:?}: {stderr}"
        );
    }
    Ok(())
}

#[test]
#[cfg(target_os = "linux")] // the forge group killer needs /proc and a system `kill`
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_held_slot_is_released_and_the_final_record_written() -> Result<(), Box<dyn Error>> {
    let reserved_at = LedgerTime::from_system_time(SystemTime::now())?;
    let month = LedgerPath::costs(reserved_at.month());
    let reserved = reserved_line(&reserved_at.to_string());
    let scene = Scene::new(LEDGER_CONFIG, &json!({ month.as_str(): reserved }))?;
    let needs = needs(&holding(), "success", &placeholder());

    let ran = scene.release(
        &["--config", CONFIG],
        &variables("1", &needs, Some(LEDGER_TOKEN)),
    )?;

    let stderr = String::from_utf8(ran.stderr)?;
    assert_eq!(ran.status.code(), Some(0), "{stderr}");
    let pushes = scene.recorded("pushes")?;
    assert!(
        pushes.len() == 1
            && pushes
                .iter()
                .all(|push| push.contains(&format!("refs/heads/slot-1:{LEASE}"))
                    && push.ends_with(":refs/heads/slot-1")),
        "one compare-and-set deletion of slot 1: {pushes:?}"
    );
    let files = scene.files()?;
    let written: String = files
        .as_object()
        .ok_or("the ledger's files are an object")?
        .values()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        written.starts_with(&reserved)
            && written.contains(r#""roles":[],"aeu":"0","kind":"final","cancelled":false"#),
        "the reservation is kept and the final record follows at the reported cost: {written}"
    );
    assert!(!stderr.contains(LEDGER_TOKEN), "{stderr}");
    Ok(())
}
