//! An in-memory GitHub contents API for one repository, answering `GET` and `PUT
//! repos/<repo>/contents/<path>` as GitHub documents them: a `PUT` with a `sha` other than the
//! file's is 409, one without a `sha` over an existing file is 422. A scripted rival writer can
//! land just before one of the adapter's writes (TST-09).

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};

use assay_app::{CostKind, CostRecord, CostRole, LedgerPath, LedgerTime, Role, RoleCost};
use assay_domain::{Aeu, RunUid, Tier};
use assay_forge::github::{Api, LedgerRepo, Method, Request, Response, Sleep, Transport};
use serde_json::{Value, json};

use super::recorded::{SEND_TIMEOUT, id, reply};

/// The adapter sent a request the contents API does not answer.
#[derive(Debug, thiserror::Error)]
#[error("unexpected request {method:?} {path}")]
pub(crate) struct Unexpected {
    method: Method,
    path: String,
}

impl Unexpected {
    fn new(method: Method, path: &str) -> Self {
        Self {
            method,
            path: path.to_owned(),
        }
    }
}

/// The ledger repository the simulator serves.
pub(crate) const LEDGER_REPOSITORY: &str = "Wet-Ink-Corporation/assay-ledger";

/// Files by path: their sha and their base64 content as last written.
#[derive(Default)]
pub(crate) struct ContentsApi {
    files: RefCell<BTreeMap<String, (String, String)>>,
    shas: Cell<u64>,
    rivals: RefCell<VecDeque<(String, Vec<u8>)>>,
    puts: Cell<usize>,
}

impl ContentsApi {
    /// Before the adapter's next `PUT`, another writer replaces `path` with `contents`.
    pub(crate) fn rival_writes(self, path: &str, contents: &[u8]) -> Self {
        self.rivals
            .borrow_mut()
            .push_back((path.to_owned(), contents.to_vec()));
        self
    }

    /// How many `PUT`s the adapter sent.
    pub(crate) fn puts(&self) -> usize {
        self.puts.get()
    }

    fn next_sha(&self) -> String {
        let next = self.shas.get().saturating_add(1);
        self.shas.set(next);
        format!("{next:040x}")
    }

    fn get(&self, path: &str) -> Response {
        match self.files.borrow().get(path) {
            Some((sha, content)) => {
                let size = decoded_len(content);
                let body = json!({
                    "type": "file",
                    "encoding": "base64",
                    "size": size,
                    "name": path.rsplit('/').next(),
                    "path": path,
                    "content": content,
                    "sha": sha,
                });
                reply(
                    200,
                    &[],
                    serde_json::to_vec(&body).expect("JSON serialises"),
                )
            }
            None => reply(404, &[], br#"{"message":"Not Found"}"#.to_vec()),
        }
    }

    fn put(&self, path: &str, body: &[u8]) -> Response {
        self.puts.set(self.puts.get().saturating_add(1));
        if let Some((rival_path, contents)) = self.rivals.borrow_mut().pop_front() {
            let sha = self.next_sha();
            self.files
                .borrow_mut()
                .insert(rival_path, (sha, encode(&contents)));
        }
        let body: Value = serde_json::from_slice(body).expect("PUT bodies are JSON");
        let current = self.files.borrow().get(path).map(|(sha, _)| sha.clone());
        let given = body.get("sha").and_then(Value::as_str);
        match (current.as_deref(), given) {
            (Some(_), None) => {
                return reply(422, &[], br#"{"message":"Invalid request."}"#.to_vec());
            }
            (Some(found), Some(sent)) if found != sent => {
                return reply(409, &[], br#"{"message":"does not match"}"#.to_vec());
            }
            (None, Some(_)) => return reply(404, &[], br#"{"message":"Not Found"}"#.to_vec()),
            _ => {}
        }
        let content = body
            .get("content")
            .and_then(Value::as_str)
            .expect("PUT bodies carry content")
            .to_owned();
        let sha = self.next_sha();
        self.files
            .borrow_mut()
            .insert(path.to_owned(), (sha.clone(), content));
        let created = if current.is_some() { 200 } else { 201 };
        let answer = json!({ "content": { "path": path, "sha": sha }, "commit": { "sha": sha } });
        reply(
            created,
            &[],
            serde_json::to_vec(&answer).expect("JSON serialises"),
        )
    }
}

impl Transport for ContentsApi {
    type Error = Unexpected;

    fn timeout(&self) -> std::time::Duration {
        SEND_TIMEOUT
    }

    fn send(&self, request: &Request<'_>) -> Result<Response, Unexpected> {
        let repository = format!("repos/{LEDGER_REPOSITORY}");
        let target = request.path.as_str();
        if request.method == Method::Get && target == repository {
            return Ok(repository_view());
        }
        let prefix = format!("{repository}/contents/");
        let Some(path) = target.strip_prefix(&prefix) else {
            return Err(Unexpected::new(request.method, target));
        };
        match (request.method, request.body) {
            (Method::Get, None) => Ok(self.get(path)),
            (Method::Put, Some(body)) => Ok(self.put(path, body)),
            _ => Err(Unexpected::new(request.method, target)),
        }
    }
}

/// `GET repos/<LEDGER_REPOSITORY>`: the token sees the repository.
fn repository_view() -> Response {
    let body = json!({ "full_name": LEDGER_REPOSITORY, "private": true });
    reply(
        200,
        &[],
        serde_json::to_vec(&body).expect("JSON serialises"),
    )
}

/// `text` as a ledger time.
pub(crate) fn at(text: &str) -> LedgerTime {
    text.parse().expect("a canonical ledger time")
}

/// The October 2026 cost file.
pub(crate) fn october() -> LedgerPath {
    LedgerPath::costs(at("2026-10-12T09:30:00.000Z").month())
}

/// `repos/<LEDGER_REPOSITORY>/contents/<path>`.
pub(crate) fn contents_path(path: &LedgerPath) -> String {
    format!("repos/{LEDGER_REPOSITORY}/contents/{path}")
}

/// The ledger adapter over `transport` and `waits`.
pub(crate) fn ledger<T: Transport, S: Sleep>(transport: T, waits: S) -> LedgerRepo<T, S> {
    LedgerRepo::new(
        Api::new(transport, waits),
        LEDGER_REPOSITORY.parse().expect("a valid repository"),
    )
}

/// The `lite` reservation of run `run_id` on PR 7 of `acme/widgets`.
pub(crate) fn reservation(run_id: u64) -> CostRecord {
    CostRecord {
        run_uid: RunUid {
            repo_id: id(42),
            run_id: id(run_id),
            run_attempt: id(1),
        },
        run_key: format!("sha256:{}", "a".repeat(64))
            .parse()
            .expect("a digest"),
        repo: "acme/widgets".to_owned(),
        pr: id(7),
        head_sha: "b".repeat(40).parse().expect("a commit id"),
        tier: Tier::Lite,
        roles: vec![RoleCost {
            role: CostRole::Role(Role::Combined),
            aeu: Aeu::from_micros(740_000),
        }],
        aeu: Aeu::from_micros(740_000),
        kind: CostKind::Reserved,
        cancelled: false,
        at: at("2026-10-12T09:30:00.000Z"),
    }
}

/// The standard alphabet.
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// `bytes` in padded standard base64, as a rival writer sends them.
pub(crate) fn encode(bytes: &[u8]) -> String {
    let mut encoded = String::new();
    for chunk in bytes.chunks(3) {
        let group = chunk
            .iter()
            .enumerate()
            .fold(0_u32, |group, (index, &byte)| {
                group | (u32::from(byte) << (16 - 8 * index))
            });
        for index in 0..4 {
            if index <= chunk.len() {
                let sextet = (group >> (18 - 6 * index)) & 0x3f;
                let sextet = usize::try_from(sextet).expect("a sextet fits usize");
                encoded.push(char::from(ALPHABET[sextet]));
            } else {
                encoded.push('=');
            }
        }
    }
    encoded
}

/// The decoded length of padded base64 `content`.
fn decoded_len(content: &str) -> usize {
    let symbols = content.bytes().filter(|&byte| byte != b'\n').count();
    let padding = content.bytes().filter(|&byte| byte == b'=').count();
    symbols / 4 * 3 - padding
}
