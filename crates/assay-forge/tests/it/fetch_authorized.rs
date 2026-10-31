//! An authenticated fetch of PR objects from a private repository (#60 ST-3a, D9): the token a
//! workflow step minted outside assay travels as `http.<remote>.extraHeader` for that remote
//! URL only, through `GIT_CONFIG_*` variables, never on the command line. A stand-in `git`
//! records each invocation's arguments and environment, so no network is used (TST-10).

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use assay_forge::git::{
    Authorization, CommitSha, GitEnvironment, GitLimits, HttpsRemote, InvalidHeader, ObjectRepo,
};
use assay_forge::github::{GhToken, InvalidToken};
use assay_forge::process::GroupKiller;
use tempfile::TempDir;

use crate::support::write_executable;

const REMOTE: &str = "https://github.com/Wet-Ink-Corporation/private-consumer.git";
const COMMIT: &str = "0671db2374a1928aa85a711de7476edd9263a48a";
const SECRET: &str = "ghs_secret";
/// `x-access-token:ghs_secret` in standard base64.
const CREDENTIALS: &str = "eC1hY2Nlc3MtdG9rZW46Z2hzX3NlY3JldA==";

/// One recorded run of the stand-in git.
#[derive(Debug)]
struct Invocation {
    args: Vec<String>,
    env: Vec<(String, String)>,
}

impl Invocation {
    fn var(&self, name: &str) -> Option<&str> {
        self.env
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// The arguments from the subcommand on.
    fn from(&self, subcommand: &str) -> Vec<&str> {
        self.args
            .iter()
            .map(String::as_str)
            .skip_while(|arg| *arg != subcommand)
            .collect()
    }
}

/// An object repository whose git is a stand-in that records every run and exits 0.
struct StandIn {
    scratch: TempDir,
    repo: ObjectRepo,
}

impl StandIn {
    fn new() -> Result<Self, Box<dyn Error>> {
        let scratch = tempfile::tempdir()?;
        let bin = scratch.path().join("bin");
        fs::create_dir(&bin)?;
        let log = scratch.path().join("git.log");
        write_executable(&bin.join("git"), &recorder(&log))?;
        let environment = GitEnvironment::new(bin.into_os_string(), GroupKiller::system()?);
        let repo = ObjectRepo::init(
            scratch.path().join("objects.git"),
            environment,
            GitLimits::default(),
        )?;
        Ok(Self { scratch, repo })
    }

    /// Every run since `init`, in order.
    fn runs(&self) -> Result<Vec<Invocation>, Box<dyn Error>> {
        let log = fs::read_to_string(self.log())?;
        let mut runs = Vec::new();
        for record in log.split_terminator("\0end\n") {
            let (args, env) = record.split_once("\0env\n").ok_or("a run's environment")?;
            runs.push(Invocation {
                args: args.split_terminator('\0').map(str::to_owned).collect(),
                env: env
                    .lines()
                    .filter_map(|line| line.split_once('='))
                    .map(|(key, value)| (key.to_owned(), value.to_owned()))
                    .collect(),
            });
        }
        // The first run is `init`'s.
        Ok(runs.into_iter().skip(1).collect())
    }

    fn log(&self) -> PathBuf {
        self.scratch.path().join("git.log")
    }
}

/// A shell script appending its NUL-separated arguments, then its environment, to `log`.
fn recorder(log: &Path) -> String {
    format!(
        "#!/bin/sh\n{{\n  for arg in \"$@\"; do printf '%s\\0' \"$arg\"; done\n  printf '\\0env\\n'\n  /usr/bin/env\n  printf '\\0end\\n'\n}} >> '{}'\n",
        log.display()
    )
}

fn token() -> Result<GhToken, InvalidToken> {
    GhToken::new(SECRET)
}

#[test]
fn an_authorized_fetch_sends_its_header_to_that_url_only() -> Result<(), Box<dyn Error>> {
    let stand_in = StandIn::new()?;
    let remote: HttpsRemote = REMOTE.parse()?;
    let commit: CommitSha = COMMIT.parse()?;
    let authorization = token()?.git_authorization()?;

    stand_in
        .repo
        .fetch_authorized(&remote, &authorization, &commit)?;

    let runs = stand_in.runs()?;
    let [fetch, verify] = runs.as_slice() else {
        return Err(format!("a fetch and its check, not {runs:?}").into());
    };
    assert_eq!(
        ["GIT_CONFIG_COUNT", "GIT_CONFIG_KEY_0", "GIT_CONFIG_VALUE_0"].map(|key| fetch.var(key)),
        [
            Some("1"),
            Some(format!("http.{REMOTE}.extraHeader").as_str()),
            Some(format!("Authorization: basic {CREDENTIALS}").as_str()),
        ]
    );
    assert!(
        fetch
            .args
            .iter()
            .all(|arg| !arg.contains(SECRET) && !arg.contains(CREDENTIALS)),
        "{fetch:?}"
    );
    assert!(
        fetch
            .args
            .windows(2)
            .any(|pair| pair == ["-c", "http.followRedirects=false"]),
        "the credential follows no redirect: {fetch:?}"
    );
    assert_eq!(
        fetch.from("fetch"),
        [
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-recurse-submodules",
            "--no-write-fetch-head",
            "--no-auto-gc",
            "--no-auto-maintenance",
            REMOTE,
            &format!("{COMMIT}:refs/assay/fetched/{COMMIT}"),
        ]
    );
    assert_eq!(
        verify.var("GIT_CONFIG_COUNT"),
        None,
        "only the fetch carries the header: {verify:?}"
    );
    Ok(())
}

#[test]
fn a_plain_fetch_sends_no_header() -> Result<(), Box<dyn Error>> {
    let stand_in = StandIn::new()?;

    stand_in.repo.fetch(&REMOTE.parse()?, &COMMIT.parse()?)?;

    let runs = stand_in.runs()?;
    let [fetch, _verify] = runs.as_slice() else {
        return Err(format!("a fetch and its check, not {runs:?}").into());
    };
    assert_eq!(fetch.var("GIT_CONFIG_COUNT"), None, "{fetch:?}");
    assert_eq!(fetch.from("fetch").first(), Some(&"fetch"));
    Ok(())
}

#[test]
fn a_token_with_a_control_character_is_refused_before_git_runs() -> Result<(), Box<dyn Error>> {
    let stand_in = StandIn::new()?;

    for injected in ["ghs_a\r\nX-Evil: 1", "ghs_a\n", "ghs_\u{7f}"] {
        assert_eq!(
            GhToken::new(injected).map(drop),
            Err(InvalidToken),
            "{injected:?}"
        );
    }
    assert_eq!(
        Authorization::new("Authorization: basic eA==\r\nX-Evil: 1").map(drop),
        Err(InvalidHeader)
    );

    assert!(stand_in.runs()?.is_empty(), "git never ran");
    Ok(())
}

#[test]
fn the_authorization_never_shows_the_token() -> Result<(), Box<dyn Error>> {
    let authorization = token()?.git_authorization()?;

    let shown = format!("{authorization:?}");

    assert_eq!(shown, "Authorization(<redacted>)");
    Ok(())
}
