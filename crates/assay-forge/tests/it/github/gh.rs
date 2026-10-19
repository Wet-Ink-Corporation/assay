//! `GhApi`, the `gh api` transport, over a fake runner: the call it builds, and how it reads
//! what gh wrote and how gh exited (#273). The process itself runs in `assay-cli`, whose
//! tests drive a fake `gh` through the real supervisor.

use std::cell::RefCell;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Duration;

use assay_forge::github::{
    ApiPath, GH_OUTPUT_CAP, GhApi, GhCall, GhEnvironment, GhError, GhRunner, GhSetupError, GhToken,
    InvalidPath, InvalidToken, Request, Response, Transport,
};
use assay_forge::process::{Captured, Limits, ProcessError};

/// What the runner was asked to run, owned.
#[derive(Debug, PartialEq, Eq)]
struct Ran {
    program: PathBuf,
    args: Vec<String>,
    env: Vec<(String, OsString)>,
    cwd: PathBuf,
    cwd_was_a_directory: bool,
    cwd_mode: u32,
    stdin: Vec<u8>,
    limits: Limits,
    debug: String,
}

/// A runner that records each call and answers with what the test scripted.
#[derive(Debug)]
struct FakeGh {
    ran: RefCell<Vec<Ran>>,
    answer: RefCell<Option<Result<Captured, ProcessError>>>,
}

impl FakeGh {
    fn answering(answer: Result<Captured, ProcessError>) -> Self {
        Self {
            ran: RefCell::new(Vec::new()),
            answer: RefCell::new(Some(answer)),
        }
    }

    /// gh exiting with `code` after writing `stdout` and `stderr`.
    fn exiting(code: i32, stdout: &[u8], stderr: &[u8]) -> Self {
        Self::answering(Ok(Captured {
            status: ExitStatus::from_raw(code << 8),
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
        }))
    }
}

impl GhRunner for FakeGh {
    fn run(&self, call: &GhCall<'_>) -> Result<Captured, ProcessError> {
        self.ran.borrow_mut().push(Ran {
            program: call.program.to_path_buf(),
            args: call.args.iter().map(|&arg| arg.to_owned()).collect(),
            env: call
                .env
                .iter()
                .map(|&(name, value)| (name.to_owned(), value.to_owned()))
                .collect(),
            cwd: call.cwd.to_path_buf(),
            cwd_was_a_directory: call.cwd.is_dir(),
            cwd_mode: fs::metadata(call.cwd).map_or(0, |meta| meta.permissions().mode() & 0o777),
            stdin: call.stdin.to_vec(),
            limits: call.limits,
            debug: format!("{call:?}"),
        });
        self.answer
            .borrow_mut()
            .take()
            .unwrap_or(Err(ProcessError::TimedOut))
    }
}

type TestResult = Result<(), Box<dyn Error>>;

const GH: &str = "/opt/gh/bin/gh";
const TIMEOUT: Duration = Duration::from_secs(7);
const OK: &[u8] = b"HTTP/2.0 200 OK\nX-Ratelimit-Remaining: 9\r\n\r\n[]";

fn api(fake: &FakeGh) -> Result<GhApi<&FakeGh>, Box<dyn Error>> {
    api_in(fake, GhEnvironment::default())
}

/// The transport over `fake` with `environment` passed on.
fn api_in(fake: &FakeGh, environment: GhEnvironment) -> Result<GhApi<&FakeGh>, Box<dyn Error>> {
    Ok(GhApi::new(fake, GH, GhToken::new("ghs_secret")?, environment)?.with_timeout(TIMEOUT))
}

fn path(text: &str) -> Result<ApiPath, InvalidPath> {
    ApiPath::new(text)
}

/// `GET text` through a new transport over `fake`.
fn get(fake: &FakeGh, text: &str) -> Result<Result<Response, GhError>, Box<dyn Error>> {
    Ok(api(fake)?.send(&Request::get(&path(text)?)))
}

#[test]
fn a_read_runs_gh_api_with_exactly_its_arguments_and_environment() -> TestResult {
    let fake = FakeGh::exiting(0, OK, b"");

    let response = get(&fake, "repos/o/r/labels?per_page=100")??;

    assert_eq!(response.status, 200);
    assert_eq!(response.header("x-ratelimit-remaining"), Some("9"));
    assert_eq!(response.body, b"[]");
    let ran = fake.ran.borrow();
    let [ran] = ran.as_slice() else {
        return Err(format!("one run expected, got {ran:?}").into());
    };
    assert_eq!(ran.program, Path::new(GH));
    assert_eq!(
        ran.args,
        [
            "api",
            "--include",
            "--method",
            "GET",
            "--header",
            "Accept: application/vnd.github+json",
            "--header",
            "X-GitHub-Api-Version: 2022-11-28",
            "--",
            "repos/o/r/labels?per_page=100",
        ]
    );
    assert_eq!(
        ran.env,
        [
            ("GH_TOKEN".to_owned(), OsString::from("ghs_secret")),
            ("GH_HOST".to_owned(), OsString::from("github.com")),
            ("GH_PROMPT_DISABLED".to_owned(), OsString::from("1")),
            ("NO_COLOR".to_owned(), OsString::from("1")),
            ("GH_NO_UPDATE_NOTIFIER".to_owned(), OsString::from("1")),
            ("GH_CONFIG_DIR".to_owned(), ran.cwd.as_os_str().to_owned()),
        ]
    );
    assert!(ran.cwd_was_a_directory, "{}", ran.cwd.display());
    assert_eq!(
        ran.cwd_mode, 0o700,
        "only this user may read gh's configuration"
    );
    assert!(ran.stdin.is_empty());
    assert_eq!(
        ran.limits,
        Limits {
            timeout: TIMEOUT,
            max_output_bytes: GH_OUTPUT_CAP,
        }
    );
    Ok(())
}

#[test]
fn a_calls_debug_shows_no_variable_value() -> TestResult {
    let fake = FakeGh::exiting(0, OK, b"");
    let environment = GhEnvironment::from_vars([(
        OsString::from("HTTPS_PROXY"),
        OsString::from("http://user:proxy-password@proxy:3128"),
    )]);

    api_in(&fake, environment)?.send(&Request::get(&path("rate_limit")?))?;

    let ran = fake.ran.borrow();
    let debug = &ran.first().ok_or("one run")?.debug;
    assert!(!debug.contains("ghs_secret"), "{debug}");
    assert!(!debug.contains("proxy-password"), "{debug}");
    assert!(debug.contains("GH_TOKEN"), "names are shown: {debug}");
    Ok(())
}

#[test]
fn only_the_proxy_and_certificate_variables_pass_through() -> TestResult {
    let fake = FakeGh::exiting(0, OK, b"");
    let variables = [
        ("HOME", "/root"),
        ("PATH", "/usr/bin"),
        ("no_proxy", "localhost"),
        ("GH_TOKEN", "ghs_inherited"),
        ("GH_HOST", "evil.example"),
        ("GH_ENTERPRISE_TOKEN", "ghe_inherited"),
        ("SSL_CERT_FILE", "/etc/ssl/ca.pem"),
        ("HTTPS_PROXY", "http://proxy:3128"),
        ("GIT_DIR", "/elsewhere"),
    ];
    let environment = GhEnvironment::from_vars(
        variables
            .iter()
            .map(|&(name, value)| (OsString::from(name), OsString::from(value))),
    );

    api_in(&fake, environment)?.send(&Request::get(&path("rate_limit")?))?;

    let ran = fake.ran.borrow();
    let ran = ran.first().ok_or("one run")?;
    let pairs: Vec<(&str, &OsStr)> = ran
        .env
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_os_str()))
        .collect();
    assert_eq!(
        pairs,
        [
            ("GH_TOKEN", OsStr::new("ghs_secret")),
            ("GH_HOST", OsStr::new("github.com")),
            ("GH_PROMPT_DISABLED", OsStr::new("1")),
            ("NO_COLOR", OsStr::new("1")),
            ("GH_NO_UPDATE_NOTIFIER", OsStr::new("1")),
            ("GH_CONFIG_DIR", ran.cwd.as_os_str()),
            ("HTTPS_PROXY", OsStr::new("http://proxy:3128")),
            ("no_proxy", OsStr::new("localhost")),
            ("SSL_CERT_FILE", OsStr::new("/etc/ssl/ca.pem")),
        ]
    );
    Ok(())
}

#[test]
fn a_write_sends_its_body_on_stdin_as_json() -> TestResult {
    let fake = FakeGh::exiting(0, b"HTTP/2.0 201 Created\n\r\n{}", b"");

    let response = api(&fake)?.send(&Request::post(
        &path("repos/o/r/check-runs")?,
        br#"{"name":"assay"}"#,
    ))?;

    assert_eq!(response.status, 201);
    let ran = fake.ran.borrow();
    let ran = ran.first().ok_or("one run expected")?;
    assert_eq!(ran.stdin, br#"{"name":"assay"}"#);
    assert_eq!(
        ran.args.get(2..).ok_or("arguments")?,
        [
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
    Ok(())
}

#[test]
fn gh_exiting_1_on_an_error_status_is_that_response() -> TestResult {
    let fake = FakeGh::exiting(
        1,
        b"HTTP/2.0 404 Not Found\n\r\n{\"message\":\"Not Found\"}",
        b"gh: Not Found (HTTP 404)\n",
    );

    let response = get(&fake, "repos/o/r/labels/x")??;

    assert_eq!(response.status, 404);
    assert_eq!(response.body, br#"{"message":"Not Found"}"#);
    Ok(())
}

#[test]
fn gh_exiting_1_on_a_graphql_answer_is_that_response() -> TestResult {
    let fake = FakeGh::exiting(1, b"HTTP/2.0 200 OK\n\r\n{\"errors\":[]}", b"gh: x\n");

    let response = api(&fake)?.send(&Request::post(&path("graphql")?, b"{}"))?;

    assert_eq!(response.status, 200);
    Ok(())
}

#[test]
fn an_exit_that_disagrees_with_the_status_is_refused() -> TestResult {
    let not_found = b"HTTP/2.0 404 Not Found\n\r\n{}";
    for (code, stdout, request_path) in [
        (0, &not_found[..], "repos/o/r/labels"),
        (1, OK, "repos/o/r/labels"),
        (2, OK, "repos/o/r/labels"),
        (2, OK, "graphql"),
        (0, not_found, "graphql"),
    ] {
        let fake = FakeGh::exiting(code, stdout, b"");

        let sent = get(&fake, request_path)?;

        assert!(
            matches!(sent, Err(GhError::Inconsistent { .. })),
            "{code} {request_path}: {sent:?}"
        );
    }
    Ok(())
}

#[test]
fn a_signalled_gh_is_refused() -> TestResult {
    let fake = FakeGh::answering(Ok(Captured {
        status: ExitStatus::from_raw(9),
        stdout: OK.to_vec(),
        stderr: Vec::new(),
    }));

    let sent = get(&fake, "rate_limit")?;

    assert!(
        matches!(sent, Err(GhError::Inconsistent { status: 200, .. })),
        "{sent:?}"
    );
    Ok(())
}

#[test]
fn a_failure_without_a_response_keeps_a_bounded_excerpt_of_stderr() -> TestResult {
    let mut stderr = b"error connecting to api.github.com\n".to_vec();
    stderr.resize(4096, b'x');
    let fake = FakeGh::exiting(1, b"", &stderr);

    let sent = get(&fake, "rate_limit")?;

    let Err(GhError::NoResponse { stderr, .. }) = sent else {
        return Err(format!("NoResponse expected, got {sent:?}").into());
    };
    assert!(stderr.starts_with("error connecting to api.github.com\n"));
    assert_eq!(stderr.len(), 512);
    Ok(())
}

#[test]
fn the_stderr_excerpt_never_holds_the_token() -> TestResult {
    // Once early, and once across the excerpt's end.
    let mut stderr = b"bad token ghs_secret\n".to_vec();
    stderr.resize(505, b'x');
    stderr.extend_from_slice(b"ghs_secret and more");
    let fake = FakeGh::exiting(1, b"", &stderr);

    let sent = get(&fake, "rate_limit")?;

    let Err(GhError::NoResponse { stderr, .. }) = sent else {
        return Err(format!("NoResponse expected, got {sent:?}").into());
    };
    assert!(stderr.starts_with("bad token <redacted>\n"), "{stderr}");
    assert!(!stderr.contains("ghs_"), "{stderr}");
    assert!(stderr.len() <= 512);
    Ok(())
}

#[test]
fn output_that_is_not_an_included_response_is_malformed() -> TestResult {
    let fake = FakeGh::exiting(0, b"[]", b"");

    let sent = get(&fake, "rate_limit")?;

    assert!(matches!(sent, Err(GhError::Malformed { .. })), "{sent:?}");
    Ok(())
}

#[test]
fn a_runner_failure_is_a_transport_error() -> TestResult {
    let fake = FakeGh::answering(Err(ProcessError::OutputTooLarge));

    let sent = get(&fake, "rate_limit")?;

    assert!(
        matches!(sent, Err(GhError::Run(ProcessError::OutputTooLarge))),
        "{sent:?}"
    );
    Ok(())
}

#[test]
fn a_path_gh_would_fill_in_is_refused_before_gh_runs() -> TestResult {
    let fake = FakeGh::exiting(0, OK, b"");

    for text in [
        "repos/{owner}/{repo}/labels",
        "repos/o/r}",
        "repos/:owner/:repo/labels",
        "repos/o/r/branches/:branch",
        "repos/o/r/pulls?head=:owner",
        "repos/o/:repo",
        "repos/o/x:branch.y",
    ] {
        let sent = get(&fake, text)?;

        assert!(
            matches!(sent, Err(GhError::Placeholder)),
            "{text}: {sent:?}"
        );
    }
    assert!(fake.ran.borrow().is_empty());
    Ok(())
}

#[test]
fn a_colon_word_gh_does_not_fill_in_is_sent() -> TestResult {
    for text in [
        "repos/o/r/labels/:owners",
        "search/issues?q=repo:o/r",
        "repos/o/r/:repo_x",
    ] {
        let fake = FakeGh::exiting(0, OK, b"");

        let response = get(&fake, text)??;

        assert_eq!(response.status, 200, "{text}");
    }
    Ok(())
}

#[test]
fn gh_must_be_named_by_an_absolute_path() -> TestResult {
    let fake = FakeGh::exiting(0, OK, b"");

    let built = GhApi::new(
        &fake,
        "gh",
        GhToken::new("ghs_secret")?,
        GhEnvironment::default(),
    );

    assert!(
        matches!(built, Err(GhSetupError::RelativeProgram(ref program)) if program == Path::new("gh")),
        "{built:?}"
    );
    Ok(())
}

#[test]
fn a_token_is_printable_ascii_and_never_shown() -> TestResult {
    for bad in ["", "ghs token", "ghs_\n", "ghs_é"] {
        assert_eq!(GhToken::new(bad).map(|_| ()), Err(InvalidToken), "{bad:?}");
    }
    let token = GhToken::new("ghs_secret")?;
    assert!(!format!("{token:?}").contains("secret"));
    Ok(())
}

#[test]
fn closing_removes_the_scratch_configuration() -> TestResult {
    let fake = FakeGh::exiting(0, OK, b"");
    let transport = api(&fake)?;
    transport.send(&Request::get(&path("rate_limit")?))?;
    // Owned: the path outlives the borrow of the runs.
    let config = fake.ran.borrow().first().ok_or("one run")?.cwd.clone();
    assert!(config.is_dir());

    transport.close()?;

    assert!(!config.exists());
    Ok(())
}

#[test]
fn dropping_the_transport_also_removes_the_scratch_configuration() -> TestResult {
    let fake = FakeGh::exiting(0, OK, b"");
    let transport = api(&fake)?;
    transport.send(&Request::get(&path("rate_limit")?))?;
    // Owned: the path outlives the borrow of the runs.
    let config = fake.ran.borrow().first().ok_or("one run")?.cwd.clone();
    assert!(config.is_dir());

    drop(transport);

    assert!(!config.exists());
    Ok(())
}
