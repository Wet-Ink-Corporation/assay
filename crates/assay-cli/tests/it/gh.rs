//! `ForgeSupervisor` as `assay-forge`'s `GhRunner`: the `gh api` transport run under the
//! shared process-group supervisor, against a stand-in `gh` script (no network, #273).

use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::time::Duration;

use assay_app::{AppId, Forge, UserId};
use assay_cli::ForgeSupervisor;
use assay_forge::github::{
    Api, ApiPath, AppIdentity, GH_TIMEOUT, GhApi, GhError, GhToken, GitHub, Request, ThreadSleep,
    Transport,
};
use assay_forge::process::{GroupKiller, ProcessError};
use tempfile::TempDir;

use crate::support::write_executable;

/// A stand-in `gh` written from `body` (after `#!/bin/sh`) into a new directory.
struct FakeGh {
    dir: TempDir,
    program: PathBuf,
}

impl FakeGh {
    fn new(body: &str) -> Result<Self, Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        let program = dir.path().join("gh");
        write_executable(&program, &format!("#!/bin/sh\ndir=${{0%/*}}\n{body}"))?;
        Ok(Self { dir, program })
    }

    /// The transport over this `gh`, with `timeout` per request.
    fn transport(&self, timeout: Duration) -> Result<GhApi<ForgeSupervisor>, Box<dyn Error>> {
        let supervisor = ForgeSupervisor::new(GroupKiller::system()?);
        Ok(GhApi::new(
            supervisor,
            &self.program,
            GhToken::new("ghs_test")?,
            timeout,
        )?)
    }

    /// A file the script wrote beside itself.
    fn record(&self, name: &str) -> Result<String, Box<dyn Error>> {
        Ok(fs::read_to_string(self.dir.path().join(name))?)
    }

    /// The environment the script recorded in `env`, without what the shell adds itself.
    fn env(&self) -> Result<BTreeMap<String, String>, Box<dyn Error>> {
        Ok(self
            .record("env")?
            .lines()
            .filter_map(|line| line.split_once('='))
            .filter(|(name, _)| !["PWD", "SHLVL", "_"].contains(name))
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect())
    }
}

#[test]
#[cfg_attr(miri, ignore = "spawns processes; Miri cannot")]
fn gh_runs_with_exactly_the_call_forge_built() -> Result<(), Box<dyn Error>> {
    let gh = FakeGh::new(
        r#"for arg; do printf '%s\n' "$arg"; done > "$dir/args"
/usr/bin/env > "$dir/env"
pwd -P > "$dir/cwd"
/bin/cat > "$dir/stdin"
printf 'HTTP/2.0 201 Created\nX-Test: yes\r\n\r\n{"id":1}'
"#,
    )?;
    let transport = gh.transport(GH_TIMEOUT)?;
    let path = ApiPath::new("repos/o/r/check-runs")?;

    let response = transport.send(&Request::post(&path, br#"{"name":"assay"}"#))?;

    assert_eq!(response.status, 201);
    assert_eq!(response.header("x-test"), Some("yes"));
    assert_eq!(response.body, br#"{"id":1}"#);
    assert_eq!(
        gh.record("args")?.lines().collect::<Vec<_>>(),
        [
            "api",
            "--include",
            "--method",
            "POST",
            "--header",
            "Accept: application/vnd.github+json",
            "--header",
            "X-GitHub-Api-Version: 2022-11-28",
            "--header",
            "Content-Type: application/json",
            "--input",
            "-",
            "--",
            "repos/o/r/check-runs",
        ]
    );
    assert_eq!(gh.record("stdin")?, r#"{"name":"assay"}"#);
    let env = gh.env()?;
    let config = env.get("GH_CONFIG_DIR").ok_or("GH_CONFIG_DIR is set")?;
    assert_eq!(
        env.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            "GH_CONFIG_DIR",
            "GH_HOST",
            "GH_NO_UPDATE_NOTIFIER",
            "GH_PROMPT_DISABLED",
            "GH_TOKEN",
            "NO_COLOR",
        ]
    );
    assert_eq!(env.get("GH_TOKEN").map(String::as_str), Some("ghs_test"));
    assert_eq!(env.get("GH_HOST").map(String::as_str), Some("github.com"));
    assert_eq!(
        gh.record("cwd")?,
        format!("{}\n", fs::canonicalize(config)?.display())
    );
    transport.close()?;
    assert!(!Path::new(config).exists());
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns processes; Miri cannot")]
fn the_adapter_follows_pages_through_gh() -> Result<(), Box<dyn Error>> {
    let gh = FakeGh::new(
        r#"for last; do :; done
case "$last" in
'repos/o/r/issues/7/labels?per_page=100')
  printf 'HTTP/2.0 200 OK\nLink: <https://api.github.com/repos/o/r/issues/7/labels?per_page=100&page=2>; rel="next"\r\n\r\n[{"name":"a"}]' ;;
'repos/o/r/issues/7/labels?per_page=100&page=2')
  printf 'HTTP/2.0 200 OK\n\r\n[{"name":"b"}]' ;;
*) exit 3 ;;
esac
"#,
    )?;
    let identity = AppIdentity {
        app: AppId(NonZeroU64::new(1).ok_or("non-zero")?),
        bot: UserId(NonZeroU64::new(2).ok_or("non-zero")?),
    };
    let github = GitHub::new(
        Api::new(gh.transport(GH_TIMEOUT)?, ThreadSleep),
        "o/r".parse()?,
        identity,
    );

    let labels = github.labels(NonZeroU64::new(7).ok_or("non-zero")?)?;

    assert_eq!(labels, ["a", "b"]);
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns processes; Miri cannot")]
fn a_gh_that_hangs_is_stopped_at_its_timeout() -> Result<(), Box<dyn Error>> {
    let gh = FakeGh::new("exec /bin/sleep 30\n")?;
    let transport = gh.transport(Duration::from_millis(200))?;
    let path = ApiPath::new("rate_limit")?;

    let sent = transport.send(&Request::get(&path));

    assert!(
        matches!(sent, Err(GhError::Run(ProcessError::TimedOut))),
        "{sent:?}"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns processes; Miri cannot")]
fn a_gh_that_writes_past_the_cap_is_stopped() -> Result<(), Box<dyn Error>> {
    let gh = FakeGh::new("exec /usr/bin/head -c 9000000 /dev/zero\n")?;
    let transport = gh.transport(GH_TIMEOUT)?;
    let path = ApiPath::new("rate_limit")?;

    let sent = transport.send(&Request::get(&path));

    assert!(
        matches!(sent, Err(GhError::Run(ProcessError::OutputTooLarge))),
        "{sent:?}"
    );
    Ok(())
}
