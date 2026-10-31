//! A [`Transport`] that replays recorded GitHub responses in order and keeps every request it
//! was sent, and a [`Sleep`] that records waits instead of taking them (TST-10).
//!
//! The bodies under `tests/fixtures/github/` are REST responses recorded from this repository
//! with `gh api` (check runs, issue comments, reviews, the timeline, PRs, workflow runs, the
//! viewer), trimmed of URL noise; the `assay-verdict` App has not published yet, so its checks
//! and comments are recorded ones re-attributed to App 5371952 and its bot user. GraphQL,
//! token, label, reaction and review-comment bodies follow GitHub's documented schema, since
//! GraphQL and writes cannot be recorded here. For doctor's prerequisites (#75) the repository,
//! its rulesets and the `.github/workflows` contents are recorded; branch rules, environments,
//! branch policies, workflow permissions, collaborators, users and permissions follow the
//! documented schema, since they need admin rights or settings this repository lacks.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs;
use std::num::NonZeroU64;
use std::path::Path;
use std::time::{Duration, SystemTime};

use assay_app::{AppId, CheckUpdate, CommitId, Deadline, LedgerTime, LockedWrite, UserId};
use assay_forge::github::{Api, AppIdentity, GitHub, Method, Request, Response, Sleep, Transport};

/// The repository the recordings are of.
pub(crate) const REPOSITORY: &str = "Wet-Ink-Corporation/assay";
/// The `assay-verdict` App (`docs/decisions/0004-assay-verdict-app.md`).
pub(crate) const APP: u64 = 5_371_952;
/// The id the fixtures give the App's `[bot]` user.
pub(crate) const BOT: u64 = 2_000_000_001;
/// The head commit of PR 268, which the recordings are of.
pub(crate) const HEAD: &str = "70aa6d748be6f3884e85d294112f9addf83cee59";
/// PR 268.
pub(crate) const PR: u64 = 268;

/// One recorded exchange.
struct Exchange {
    method: Method,
    path: String,
    response: Response,
}

/// A request the adapter sent.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Sent {
    pub(crate) method: Method,
    pub(crate) path: String,
    pub(crate) body: Option<serde_json::Value>,
}

/// Recorded responses, replayed in order.
#[derive(Default)]
pub(crate) struct Recorded {
    script: RefCell<VecDeque<Exchange>>,
    sent: RefCell<Vec<Sent>>,
}

/// The adapter asked for something the script did not record.
#[derive(Debug, thiserror::Error)]
#[error("unscripted request {method:?} {path}; expected {expected}")]
pub(crate) struct Unscripted {
    method: Method,
    path: String,
    expected: String,
}

impl Recorded {
    /// Expects `method path` next and answers `response`.
    pub(crate) fn then(self, method: Method, path: &str, response: Response) -> Self {
        self.script.borrow_mut().push_back(Exchange {
            method,
            path: path.to_owned(),
            response,
        });
        self
    }

    /// Expects `method path` next and answers 200 with `body`.
    pub(crate) fn ok(self, method: Method, path: &str, body: Vec<u8>) -> Self {
        self.then(method, path, reply(200, &[], body))
    }

    /// Every request sent so far.
    pub(crate) fn sent(&self) -> Vec<Sent> {
        self.sent.borrow().clone()
    }

    /// The requests sent so far that were not reads.
    pub(crate) fn writes(&self) -> Vec<Sent> {
        self.sent()
            .into_iter()
            .filter(|sent| sent.method != Method::Get && sent.path != "graphql")
            .collect()
    }

    /// How many recorded exchanges were not asked for.
    pub(crate) fn unused(&self) -> usize {
        self.script.borrow().len()
    }
}

impl Transport for Recorded {
    type Error = Unscripted;

    fn send(&self, request: &Request<'_>) -> Result<Response, Unscripted> {
        self.sent.borrow_mut().push(Sent {
            method: request.method,
            path: request.path.as_str().to_owned(),
            body: request
                .body
                .map(|body| serde_json::from_slice(body).expect("request bodies are JSON")),
        });
        let mut script = self.script.borrow_mut();
        match script.pop_front() {
            Some(next) if next.method == request.method && next.path == request.path.as_str() => {
                Ok(next.response)
            }
            other => Err(Unscripted {
                method: request.method,
                path: request.path.as_str().to_owned(),
                expected: other.map_or_else(
                    || "nothing".to_owned(),
                    |next| format!("{:?} {}", next.method, next.path),
                ),
            }),
        }
    }

    fn timeout(&self) -> Duration {
        SEND_TIMEOUT
    }
}

/// The timeout [`Recorded`] reports for each request, as `gh api`'s default.
pub(crate) const SEND_TIMEOUT: Duration = Duration::from_secs(30);

/// The instant the [`Waits`] clock starts at: 2026-10-19T04:00:00Z.
const START_UNIX_SECONDS: u64 = 1_792_382_400;

/// Waits asked for, never taken; its clock reads [`START_UNIX_SECONDS`] plus the waits so far.
#[derive(Default)]
pub(crate) struct Waits(RefCell<Vec<Duration>>);

impl Waits {
    /// The waits asked for so far.
    pub(crate) fn taken(&self) -> Vec<Duration> {
        self.0.borrow().clone()
    }
}

impl Sleep for Waits {
    fn sleep(&self, duration: Duration) {
        self.0.borrow_mut().push(duration);
    }

    fn now(&self) -> SystemTime {
        let waited: Duration = self.0.borrow().iter().sum();
        SystemTime::UNIX_EPOCH + Duration::from_secs(START_UNIX_SECONDS) + waited
    }
}

/// The deadline `after` the [`Waits`] clock's start.
pub(crate) fn deadline_after(after: Duration) -> Deadline {
    let start = Duration::from_secs(START_UNIX_SECONDS) + after;
    let millis = u64::try_from(start.as_millis()).expect("a test deadline fits");
    Deadline::at(LedgerTime::from_unix_millis(millis).expect("a test deadline is in range"))
}

/// `update` as a write under the publish lock whose deadline no test reaches: an hour after
/// the [`Waits`] clock's start.
pub(crate) fn locked(update: &CheckUpdate) -> LockedWrite<'_> {
    LockedWrite {
        update,
        deadline: deadline_after(Duration::from_secs(3_600)),
    }
}

/// A response of `status` with `headers` and `body`.
pub(crate) fn reply(status: u16, headers: &[(&str, &str)], body: Vec<u8>) -> Response {
    Response {
        status,
        headers: headers
            .iter()
            .map(|&(name, value)| (name.to_owned(), value.to_owned()))
            .collect(),
        body,
    }
}

/// The bytes of fixture `name`.
pub(crate) fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/github")
        .join(name);
    fs::read(&path).unwrap_or_else(|error| panic!("fixture {}: {error}", path.display()))
}

/// Fixture `name` as JSON, to derive a variant from.
pub(crate) fn fixture_json(name: &str) -> serde_json::Value {
    serde_json::from_slice(&fixture(name)).expect("fixtures are JSON")
}

/// The body of the first write sent.
pub(crate) fn first_write(recorded: &Recorded) -> Option<serde_json::Value> {
    recorded
        .writes()
        .into_iter()
        .next()
        .and_then(|sent| sent.body)
}

/// The bodies of every write sent, in order.
pub(crate) fn write_bodies(recorded: &Recorded) -> Vec<Option<serde_json::Value>> {
    recorded
        .writes()
        .into_iter()
        .map(|sent| sent.body)
        .collect()
}

/// `value` as a JSON body.
pub(crate) fn json(value: &serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(value).expect("a JSON value serialises")
}

/// The App the fixtures are for.
pub(crate) fn identity() -> AppIdentity {
    AppIdentity {
        app: AppId(NonZeroU64::new(APP).expect("non-zero")),
        bot: UserId(NonZeroU64::new(BOT).expect("non-zero")),
    }
}

/// The adapter over `recorded` and `waits`, for [`REPOSITORY`].
pub(crate) fn github<'a>(
    recorded: &'a Recorded,
    waits: &'a Waits,
) -> GitHub<&'a Recorded, &'a Waits> {
    GitHub::new(
        Api::new(recorded, waits),
        REPOSITORY.parse().expect("valid repository"),
        identity(),
    )
}

/// [`HEAD`] as a commit id.
pub(crate) fn head() -> CommitId {
    HEAD.parse().expect("a full commit id")
}

/// The head of PR 267, the commit before [`HEAD`] in the recordings.
pub(crate) fn parent() -> CommitId {
    "ca42d86e668fbb18dab3ea4255fcd09518412f3b"
        .parse()
        .expect("a full commit id")
}

/// `id` as a non-zero id.
pub(crate) fn id(id: u64) -> NonZeroU64 {
    NonZeroU64::new(id).expect("fixture ids are non-zero")
}

/// The App's id.
pub(crate) fn app() -> AppId {
    AppId(id(APP))
}

/// The owner's account (`docs/decisions/0005-agent-identity-and-approver.md`).
pub(crate) fn owner() -> UserId {
    UserId(id(124_330_161))
}

/// [`PR`] as a PR number.
pub(crate) fn pr() -> NonZeroU64 {
    NonZeroU64::new(PR).expect("non-zero")
}

/// `repos/<REPOSITORY>/<rest>`.
pub(crate) fn path(rest: &str) -> String {
    format!("repos/{REPOSITORY}/{rest}")
}
