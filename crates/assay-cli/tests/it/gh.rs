//! `ForgeSupervisor` as `assay-forge`'s `GhRunner`: the `gh api` transport run under the
//! shared process-group supervisor, against a stand-in `gh` script (no network, #273), and the
//! run bundle's `gh run download` under the same supervisor and environment (wi-6b26d6).

use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::time::Duration;

use assay_app::{AppId, BundleSource, Forge, StoredBundle, UserId};
use assay_cli::ForgeSupervisor;
use assay_domain::RunUid;
use assay_forge::github::{
    Api, ApiPath, AppIdentity, ForgeError, GH_TIMEOUT, GhApi, GhEnvironment, GhError, GhToken,
    GitHub, MAX_RESPONSE_BYTES, Operation, Request, ThreadSleep, Transport,
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

    /// The transport over this `gh`, with `timeout` per request and nothing inherited.
    fn transport(&self, timeout: Duration) -> Result<GhApi<ForgeSupervisor>, Box<dyn Error>> {
        self.transport_in(timeout, GhEnvironment::default())
    }

    /// The transport over this `gh`, passing on `environment`.
    fn transport_in(
        &self,
        timeout: Duration,
        environment: GhEnvironment,
    ) -> Result<GhApi<ForgeSupervisor>, Box<dyn Error>> {
        let supervisor = ForgeSupervisor::new(GroupKiller::system()?);
        let token = GhToken::new("ghs_test")?;
        Ok(GhApi::new(supervisor, &self.program, token, environment)?.with_timeout(timeout))
    }

    /// A file the script wrote beside itself.
    fn record(&self, name: &str) -> Result<String, Box<dyn Error>> {
        Ok(fs::read_to_string(self.dir.path().join(name))?)
    }

    /// The environment the script recorded in `env`, without what the shell adds itself.
    fn env(&self) -> Result<BTreeMap<String, String>, Box<dyn Error>> {
        self.env_in("env")
    }

    /// The environment the script recorded in `name`, without what the shell adds itself.
    fn env_in(&self, name: &str) -> Result<BTreeMap<String, String>, Box<dyn Error>> {
        Ok(self
            .record(name)?
            .lines()
            .filter_map(|line| line.split_once('='))
            .filter(|(name, _)| !["PWD", "SHLVL", "_"].contains(name))
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect())
    }
}

/// What gh would inherit from a process whose environment is `vars`.
fn inheriting(vars: &[(&str, &str)]) -> GhEnvironment {
    GhEnvironment::from_vars(
        vars.iter()
            .map(|&(name, value)| (name.into(), value.into())),
    )
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
    let environment = inheriting(&[("HOME", "/root"), ("SSL_CERT_DIR", "/etc/ssl/certs")]);
    let transport = gh.transport_in(GH_TIMEOUT, environment)?;
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
    let expected = [
        ("GH_CONFIG_DIR", config.as_str()),
        ("GH_HOST", "github.com"),
        ("GH_NO_UPDATE_NOTIFIER", "1"),
        ("GH_PROMPT_DISABLED", "1"),
        ("GH_TOKEN", "ghs_test"),
        ("NO_COLOR", "1"),
        ("SSL_CERT_DIR", "/etc/ssl/certs"),
    ];
    assert!(
        env.iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .eq(expected)
    );
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
fn a_body_one_byte_over_the_response_cap_reaches_the_adapter_and_is_refused()
-> Result<(), Box<dyn Error>> {
    let over = MAX_RESPONSE_BYTES
        .checked_add(1)
        .ok_or("the cap plus one fits")?;
    let gh = FakeGh::new(&format!(
        "printf 'HTTP/2.0 200 OK\\n\\r\\n'\nexec /usr/bin/head -c {over} /dev/zero\n"
    ))?;
    let identity = AppIdentity {
        app: AppId(NonZeroU64::new(1).ok_or("non-zero")?),
        bot: UserId(NonZeroU64::new(2).ok_or("non-zero")?),
    };
    let github = GitHub::new(
        Api::new(gh.transport(GH_TIMEOUT)?, ThreadSleep),
        "o/r".parse()?,
        identity,
    );

    let listed = github.labels(NonZeroU64::new(7).ok_or("non-zero")?);

    assert!(
        matches!(
            listed,
            Err(ForgeError::ResponseTooLarge {
                operation: Operation::ListLabels,
                cap: MAX_RESPONSE_BYTES,
            })
        ),
        "{listed:?}"
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

#[test]
#[cfg_attr(miri, ignore = "spawns processes; Miri cannot")]
fn a_bundle_is_downloaded_through_gh_with_the_api_calls_environment() -> Result<(), Box<dyn Error>>
{
    let gh = FakeGh::new(
        r#"if [ "$1" = run ]; then
  /usr/bin/env > "$dir/download-env"
  pwd -P > "$dir/download-cwd"
  while [ $# -gt 0 ]; do
    if [ "$1" = --dir ]; then dest="$2"; fi
    shift
  done
  printf '%s' "$dest" > "$dir/dest"
  printf '{"bundle":1}' > "$dest/run-inputs.json"
  exit 0
fi
/usr/bin/env > "$dir/env"
printf 'HTTP/2.0 200 OK\n\r\n{"total_count":1,"artifacts":[{"name":"assay-decide-0123456789abcdef0123456789abcdef","size_in_bytes":40,"expired":false,"workflow_run":{"id":7,"repository_id":5}}]}'
"#,
    )?;
    let environment = inheriting(&[("HTTPS_PROXY", "http://proxy.invalid:3128")]);
    let identity = AppIdentity {
        app: AppId(NonZeroU64::new(1).ok_or("non-zero")?),
        bot: UserId(NonZeroU64::new(2).ok_or("non-zero")?),
    };
    let github = GitHub::new(
        Api::new(gh.transport_in(GH_TIMEOUT, environment)?, ThreadSleep),
        "o/r".parse()?,
        identity,
    );
    let run = RunUid {
        repo_id: NonZeroU64::new(5).ok_or("non-zero")?,
        run_id: NonZeroU64::new(7).ok_or("non-zero")?,
        run_attempt: NonZeroU64::MIN,
    };

    let fetched = github.fetch(run, "run-inputs.json", 1024)?;

    assert_eq!(fetched, StoredBundle::Found(br#"{"bundle":1}"#.to_vec()));
    let env = gh.env()?;
    assert_eq!(gh.env_in("download-env")?, env);
    assert_eq!(
        env.get("HTTPS_PROXY").map(String::as_str),
        Some("http://proxy.invalid:3128")
    );
    let config = env.get("GH_CONFIG_DIR").ok_or("GH_CONFIG_DIR is set")?;
    assert_eq!(
        gh.record("download-cwd")?,
        format!("{}\n", fs::canonicalize(config)?.display())
    );
    assert!(!Path::new(&gh.record("dest")?).exists());
    github.into_api().into_transport().close()?;
    Ok(())
}
