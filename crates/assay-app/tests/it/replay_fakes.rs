//! An in-memory artifact store for `replay_decision`, and a freshly persisted bundle to replay.

use std::cell::RefCell;
use std::error::Error;

use assay_app::{BundleSource, EvidenceEntry, StoredBundle};
use assay_domain::RunUid;

use crate::decide_fakes::Scenario;
use crate::plan_fakes::FakeError;
use crate::publish_support::{Run, stamped};

/// What the store holds for every run it is asked about.
#[derive(Debug, Clone)]
pub(crate) enum Stored {
    /// These bytes, whatever the limit.
    Bytes(Vec<u8>),
    /// No artifact or file.
    Missing,
    /// The artifact is past its retention.
    Expired,
    /// The adapter refused to read past the limit.
    TooLarge,
    /// The store cannot be reached.
    Fails,
}

/// A store answering every fetch with `stored`, and recording each fetch.
#[derive(Debug)]
pub(crate) struct FakeBundles {
    pub(crate) stored: Stored,
    /// Every fetch: the run, the file and the limit.
    pub(crate) asked: RefCell<Vec<(RunUid, String, usize)>>,
}

impl FakeBundles {
    pub(crate) const fn new(stored: Stored) -> Self {
        Self {
            stored,
            asked: RefCell::new(Vec::new()),
        }
    }
}

impl BundleSource for FakeBundles {
    type Error = FakeError;

    fn fetch(&self, run: RunUid, file: &str, limit: usize) -> Result<StoredBundle, FakeError> {
        self.asked.borrow_mut().push((run, file.to_owned(), limit));
        match &self.stored {
            Stored::Bytes(bytes) => Ok(StoredBundle::Found(bytes.clone())),
            Stored::Missing => Ok(StoredBundle::Missing),
            Stored::Expired => Ok(StoredBundle::Expired),
            Stored::TooLarge => Ok(StoredBundle::TooLarge),
            Stored::Fails => Err(FakeError),
        }
    }
}

/// A bundle `publish` persisted, with the evidence entry it recorded for it.
#[derive(Debug)]
pub(crate) struct Persisted {
    pub(crate) run: Run,
    pub(crate) bytes: Vec<u8>,
    pub(crate) entry: EvidenceEntry,
}

impl Persisted {
    /// The default scenario's run, published and persisted.
    pub(crate) fn new() -> Result<Self, Box<dyn Error>> {
        Self::of(&Scenario::new()?)
    }

    /// `scenario`'s run, published and persisted.
    pub(crate) fn of(scenario: &Scenario) -> Result<Self, Box<dyn Error>> {
        let run = Run::of(scenario)?;
        let world = stamped()?;
        run.publish(&world)?;
        let uploads = world.uploads.borrow();
        let [(_, _, bytes)] = uploads.as_slice() else {
            return Err(format!("expected one upload, got {}", uploads.len()).into());
        };
        let evidence = world.evidence.borrow();
        let [entry] = evidence.as_slice() else {
            return Err(format!("expected one evidence entry, got {}", evidence.len()).into());
        };
        Ok(Self {
            bytes: bytes.clone(),
            entry: entry.clone(),
            run,
        })
    }
}
