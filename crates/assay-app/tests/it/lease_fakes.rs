//! An in-memory ledger ref store: refs created only when absent, deleted only when still at the
//! commit given, as GitHub (A-26) and receive-pack (A-33) do, with scripted rivals that act just
//! before one of our calls (TST-09). Shared behind a `Mutex`, so racing threads see one store.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Mutex, MutexGuard, PoisonError};

use assay_app::{CommitId, LeaseRef, LedgerRefs, RefCreation, RefDeletion, RefTip};
use assay_domain::Aeu;

use crate::ledger_fakes::{id, run};

/// The ref store could not be reached (scripted).
#[derive(Debug, thiserror::Error)]
#[error("the in-memory ref store is down")]
pub(crate) struct RefsDown;

/// The port call a rival acts before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Before {
    Create,
    Read,
    Delete,
}

/// One call the use case made, with what it did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Call {
    Create { name: String, created: bool },
    Read { name: String },
    Delete { name: String, deleted: bool },
}

type Rival = Box<dyn FnOnce(&mut Store) + Send>;

/// The refs by name, each at a commit with its message.
#[derive(Default)]
pub(crate) struct Store {
    refs: BTreeMap<String, (CommitId, String)>,
    commits: u64,
}

impl Store {
    /// Creates `name` at a fresh commit with `message`, unless it exists.
    pub(crate) fn create(&mut self, name: &str, message: &str) -> Option<CommitId> {
        if self.refs.contains_key(name) {
            return None;
        }
        self.commits += 1;
        let commit: CommitId = format!("{:040x}", self.commits)
            .parse()
            .expect("40 hex digits are a commit id");
        self.refs
            .insert(name.to_owned(), (commit.clone(), message.to_owned()));
        Some(commit)
    }

    /// Deletes `name` whatever it points at.
    pub(crate) fn remove(&mut self, name: &str) {
        self.refs.remove(name);
    }
}

#[derive(Default)]
struct State {
    store: Store,
    rivals: VecDeque<(Before, Rival)>,
    calls: Vec<Call>,
    down: bool,
}

/// The fake ledger ref store.
#[derive(Default)]
pub(crate) struct MemoryRefs {
    state: Mutex<State>,
}

impl MemoryRefs {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A store holding `name` at a fresh commit with `message`; returns the commit.
    pub(crate) fn holding(&self, name: &str, message: &str) -> CommitId {
        self.lock()
            .store
            .create(name, message)
            .expect("the ref is new")
    }

    /// Before our next call of kind `before`, `rival` acts on the store.
    pub(crate) fn rival(
        self,
        before: Before,
        rival: impl FnOnce(&mut Store) + Send + 'static,
    ) -> Self {
        self.lock().rivals.push_back((before, Box::new(rival)));
        self
    }

    /// Every call fails.
    pub(crate) fn down(self) -> Self {
        self.lock().down = true;
        self
    }

    /// The ref `name`: its commit and message.
    pub(crate) fn get(&self, name: &str) -> Option<(CommitId, String)> {
        self.lock().store.refs.get(name).cloned()
    }

    /// The names of every ref, in order.
    pub(crate) fn names(&self) -> Vec<String> {
        self.lock().store.refs.keys().cloned().collect()
    }

    /// Every call made, in order.
    pub(crate) fn calls(&self) -> Vec<Call> {
        self.lock().calls.clone()
    }

    /// Runs the first rival waiting for `before`, then `call`, all under one lock, so a call is
    /// atomic as GitHub's and receive-pack's are.
    fn with<T>(&self, before: Before, call: impl FnOnce(&mut State) -> T) -> Result<T, RefsDown> {
        locked(&mut self.lock(), before, call)
    }
}

fn locked<T>(
    state: &mut State,
    before: Before,
    call: impl FnOnce(&mut State) -> T,
) -> Result<T, RefsDown> {
    if state.down {
        return Err(RefsDown);
    }
    if let Some(index) = state.rivals.iter().position(|(when, _)| *when == before)
        && let Some((_, rival)) = state.rivals.remove(index)
    {
        rival(&mut state.store);
    }
    Ok(call(state))
}

/// Slot `index`, 0 or 1.
pub(crate) fn slot(index: u8) -> assay_app::Slot {
    assay_app::Slot::new(index).expect("slots 0 and 1 exist")
}

impl LedgerRefs for MemoryRefs {
    type Error = RefsDown;

    fn create(&self, name: &LeaseRef, message: &str) -> Result<RefCreation, RefsDown> {
        self.with(Before::Create, |state| {
            let created = state.store.create(name.as_str(), message);
            state.calls.push(Call::Create {
                name: name.as_str().to_owned(),
                created: created.is_some(),
            });
            created.map_or(RefCreation::Exists, RefCreation::Created)
        })
    }

    fn read(&self, name: &LeaseRef) -> Result<Option<RefTip>, RefsDown> {
        self.with(Before::Read, |state| {
            state.calls.push(Call::Read {
                name: name.as_str().to_owned(),
            });
            state
                .store
                .refs
                .get(name.as_str())
                .map(|(commit, message)| RefTip {
                    commit: commit.clone(),
                    message: message.clone(),
                })
        })
    }

    fn delete_if(&self, name: &LeaseRef, expected: &CommitId) -> Result<RefDeletion, RefsDown> {
        self.with(Before::Delete, |state| {
            let at_expected = state
                .store
                .refs
                .get(name.as_str())
                .is_some_and(|(commit, _)| commit == expected);
            if at_expected {
                state.store.refs.remove(name.as_str());
            }
            state.calls.push(Call::Delete {
                name: name.as_str().to_owned(),
                deleted: at_expected,
            });
            if at_expected {
                RefDeletion::Deleted
            } else {
                RefDeletion::Moved
            }
        })
    }
}

/// The slot request of run `run_id` of repository 42 on PR 7 of `acme/widgets`, reserving 2.3.
pub(crate) fn slot_request(run_id: u64) -> assay_app::SlotRequest {
    assay_app::SlotRequest {
        run_uid: run(run_id),
        repo: "acme/widgets".to_owned(),
        pr: id(7),
        reserved_aeu: Aeu::from_micros(2_300_000),
    }
}

/// The lease line `slot_request(run_id)` is written as, expiring at `expires_at`.
pub(crate) fn lease_line(run_id: u64, expires_at: &str) -> String {
    format!(
        r#"{{"schema":"assay.ledger-lease/1","run_uid":{{"repo_id":42,"run_id":{run_id},"run_attempt":1}},"repo":"acme/widgets","pr":7,"reserved_aeu":"2.3","expires_at":"{expires_at}"}}"#
    )
}
