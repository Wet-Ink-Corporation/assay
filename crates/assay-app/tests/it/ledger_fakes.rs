//! An in-memory ledger repository: files with versions, written by compare-and-set, with a
//! scripted rival writer that lands just before one of ours (TST-09).

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::num::NonZeroU64;

use assay_app::{
    CommitId, CostKind, CostRecord, CostRole, FileVersion, Ledger, LedgerFile, LedgerPath,
    LedgerTime, Role, RoleCost, Sha256Digest, WriteCondition, WriteOutcome,
};
use assay_domain::{Aeu, RunUid, Tier};

/// The ledger could not be reached (scripted).
#[derive(Debug, thiserror::Error)]
#[error("the in-memory ledger is down")]
pub(crate) struct Down;

/// One write the use case asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WriteCall {
    pub(crate) path: String,
    pub(crate) condition: WriteCondition,
    pub(crate) outcome: WriteOutcome,
}

/// Files by path, each with the version it was last written at.
#[derive(Default)]
pub(crate) struct MemoryLedger {
    files: RefCell<BTreeMap<String, (FileVersion, Vec<u8>)>>,
    versions: Cell<u64>,
    writes: RefCell<Vec<WriteCall>>,
    rivals: RefCell<VecDeque<Rival>>,
    down: Cell<bool>,
}

/// What another writer does just before one of our writes lands.
enum Rival {
    /// Appends `bytes` to the file, once.
    Append(Vec<u8>),
    /// Replaces the file with `bytes`, once.
    Replace(Vec<u8>),
    /// Touches the file before every one of our writes, forever.
    Always,
}

impl MemoryLedger {
    /// A ledger holding `contents` at `path`.
    pub(crate) fn with_file(self, path: &LedgerPath, contents: &[u8]) -> Self {
        let version = self.next_version();
        self.files
            .borrow_mut()
            .insert(path.as_str().to_owned(), (version, contents.to_vec()));
        self
    }

    /// Before our next write, another writer appends `bytes` to the file we write.
    pub(crate) fn rival_appends(self, bytes: &[u8]) -> Self {
        self.rivals
            .borrow_mut()
            .push_back(Rival::Append(bytes.to_vec()));
        self
    }

    /// Before our next write, another writer replaces the file we write with `bytes`.
    pub(crate) fn rival_writes(self, bytes: &[u8]) -> Self {
        self.rivals
            .borrow_mut()
            .push_back(Rival::Replace(bytes.to_vec()));
        self
    }

    /// Before every one of our writes, another writer rewrites the file unchanged.
    pub(crate) fn rival_always(self) -> Self {
        self.rivals.borrow_mut().push_back(Rival::Always);
        self
    }

    /// Every read and write fails.
    pub(crate) fn down(self) -> Self {
        self.down.set(true);
        self
    }

    /// The contents of `path`, if any.
    pub(crate) fn file(&self, path: &LedgerPath) -> Option<Vec<u8>> {
        self.files
            .borrow()
            .get(path.as_str())
            .map(|(_, contents)| contents.clone())
    }

    /// The contents of `path` as text, empty when absent.
    pub(crate) fn text(&self, path: &LedgerPath) -> String {
        String::from_utf8(self.file(path).unwrap_or_default()).expect("the fake holds text")
    }

    /// Every write asked for so far.
    pub(crate) fn writes(&self) -> Vec<WriteCall> {
        self.writes.borrow().clone()
    }

    fn next_version(&self) -> FileVersion {
        let next = self.versions.get().saturating_add(1);
        self.versions.set(next);
        FileVersion::new(format!("v{next}"))
    }

    /// Lets the next scripted rival write `path`.
    fn rival(&self, path: &str) {
        let mut rivals = self.rivals.borrow_mut();
        let Some(rival) = rivals.pop_front() else {
            return;
        };
        let version = self.next_version();
        let mut files = self.files.borrow_mut();
        let current = files.get(path).map(|(_, contents)| contents.clone());
        let mut contents = current.unwrap_or_default();
        match rival {
            Rival::Append(bytes) => contents.extend_from_slice(&bytes),
            Rival::Replace(bytes) => contents = bytes,
            Rival::Always => rivals.push_front(Rival::Always),
        }
        files.insert(path.to_owned(), (version, contents));
    }
}

impl Ledger for MemoryLedger {
    type Error = Down;

    fn read(&self, path: &LedgerPath) -> Result<Option<LedgerFile>, Down> {
        if self.down.get() {
            return Err(Down);
        }
        Ok(self
            .files
            .borrow()
            .get(path.as_str())
            .map(|(version, contents)| LedgerFile {
                version: version.clone(),
                contents: contents.clone(),
            }))
    }

    fn write(
        &self,
        path: &LedgerPath,
        contents: &[u8],
        condition: &WriteCondition,
    ) -> Result<WriteOutcome, Down> {
        if self.down.get() {
            return Err(Down);
        }
        self.rival(path.as_str());
        let current = self
            .files
            .borrow()
            .get(path.as_str())
            .map(|(version, _)| version.clone());
        let holds = match (condition, current) {
            (WriteCondition::Absent, None) => true,
            (WriteCondition::At(expected), Some(found)) => *expected == found,
            _ => false,
        };
        let outcome = if holds {
            let version = self.next_version();
            self.files
                .borrow_mut()
                .insert(path.as_str().to_owned(), (version, contents.to_vec()));
            WriteOutcome::Written
        } else {
            WriteOutcome::Conflict
        };
        self.writes.borrow_mut().push(WriteCall {
            path: path.as_str().to_owned(),
            condition: condition.clone(),
            outcome,
        });
        Ok(outcome)
    }
}

/// A 40-hex commit id of one repeated digit.
pub(crate) fn commit(digit: char) -> CommitId {
    digit
        .to_string()
        .repeat(40)
        .parse()
        .expect("40 hex digits are a commit id")
}

/// A `sha256:` digest of one repeated digit.
pub(crate) fn digest(digit: char) -> Sha256Digest {
    format!("sha256:{}", digit.to_string().repeat(64))
        .parse()
        .expect("64 hex digits are a digest")
}

/// `n` as a non-zero id.
pub(crate) fn id(n: u64) -> NonZeroU64 {
    NonZeroU64::new(n).expect("ids in tests are positive")
}

/// The run `run_id` of repository 42.
pub(crate) fn run(run_id: u64) -> RunUid {
    RunUid {
        repo_id: id(42),
        run_id: id(run_id),
        run_attempt: id(1),
    }
}

/// `text` as a ledger time.
pub(crate) fn at(text: &str) -> LedgerTime {
    text.parse().expect("tests write canonical ledger times")
}

/// A `lite` cost record of run `run_id` on PR 7 of `acme/widgets`, written at `when`.
pub(crate) fn cost(run_id: u64, kind: CostKind, micros: u64, when: &str) -> CostRecord {
    CostRecord {
        run_uid: run(run_id),
        run_key: digest('a'),
        repo: "acme/widgets".to_owned(),
        pr: id(7),
        head_sha: commit('b'),
        tier: Tier::Lite,
        roles: vec![RoleCost {
            role: CostRole::Role(Role::Combined),
            aeu: Aeu::from_micros(micros),
        }],
        aeu: Aeu::from_micros(micros),
        kind,
        cancelled: false,
        at: at(when),
    }
}

/// The JSON line `cost(run_id, kind, micros, when)` is written as, with `aeu` its text.
pub(crate) fn cost_line(run_id: u64, kind: &str, aeu: &str, when: &str) -> String {
    format!(
        concat!(
            r#"{{"schema":"assay.ledger-cost/1","run_uid":{{"repo_id":42,"run_id":{run_id},"#,
            r#""run_attempt":1}},"run_key":"sha256:{key}","repo":"acme/widgets","pr":7,"#,
            r#""head_sha":"{head}","tier":"lite","roles":[{{"role":"combined","aeu":"{aeu}"}}],"#,
            r#""aeu":"{aeu}","kind":"{kind}","cancelled":false,"at":"{when}"}}"#,
            "\n"
        ),
        run_id = run_id,
        key = "a".repeat(64),
        head = "b".repeat(40),
        aeu = aeu,
        kind = kind,
        when = when,
    )
}
