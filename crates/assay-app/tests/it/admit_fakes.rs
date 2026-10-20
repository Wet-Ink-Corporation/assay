//! Fakes and builders for `admit` (TST-09, TST-14): the plan's run description, a clock that
//! never sleeps, a comment store, an event sink, and one world holding every port.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU64;
use std::time::{Duration, SystemTime};

use assay_app::{
    Actor, AdmitFailure, AdmitPorts, AdmitRequest, Admitted, Carry, Clock, CommandComments,
    CommentId, CommitId, CommitIdentity, ContractValidator, CostKind, CostRecord, CostRole,
    DeferralRecord, EventSink, Identities, IssueComment, LeaseRef, LedgerTime, PrState,
    RangeIdentities, RetryGrant, Role, RoleCost, Slot, UserId, admit, append_cost, grant_retry,
    read_state, update_state,
};
use assay_domain::{Aeu, Event, RunUid, Tier};
use serde_json::{Value, json};

use crate::lease_fakes::MemoryRefs;
use crate::ledger_fakes::MemoryLedger;
use crate::plan_fakes::FakeError;
use crate::replay_fakes::{FakeBundles, Stored};

/// The repository every scenario reviews, the bundle fixtures' repository.
pub(crate) const REPO: &str = "Wet-Ink-Corporation/assay";

/// Its numeric id.
pub(crate) const REPO_ID: u64 = 840_213_377;

/// The PR reviewed.
pub(crate) const PR: u64 = 42;

/// When the scenarios run.
pub(crate) const NOW: &str = "2026-10-19T12:00:00.000Z";

/// The PR's author.
pub(crate) const AUTHOR: u64 = 501;

/// The owner, a privileged identity of base `.assay/owners`.
pub(crate) const OWNER: u64 = 500;

/// An agent identity of base `.assay/agents`.
pub(crate) const AGENT: u64 = 502;

/// `n` as a non-zero id.
pub(crate) fn nz(n: u64) -> NonZeroU64 {
    NonZeroU64::new(n).expect("ids in tests are positive")
}

/// This workflow run, at `attempt`.
pub(crate) fn this_run(attempt: u64) -> RunUid {
    RunUid {
        repo_id: nz(REPO_ID),
        run_id: nz(9),
        run_attempt: nz(attempt),
    }
}

/// The run key every plan carries.
pub(crate) fn run_key() -> String {
    format!("sha256:{}", "e".repeat(64))
}

/// The PR's head.
pub(crate) fn head() -> String {
    "b".repeat(40)
}

/// `text` as a ledger time.
pub(crate) fn at(text: &str) -> LedgerTime {
    text.parse().expect("tests write canonical ledger times")
}

/// `NOW` as the clock reads it.
pub(crate) fn now_system() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_millis(at(NOW).unix_millis())
}

/// The plan's `assay.run/1` document, built with the roles of its tier.
#[derive(Debug, Clone)]
pub(crate) struct PlanDoc {
    pub(crate) tier: &'static str,
    pub(crate) classes: Vec<&'static str>,
    pub(crate) roles: Vec<&'static str>,
    pub(crate) draft: bool,
    pub(crate) run_uid: RunUid,
    pub(crate) budget: Option<Value>,
}

/// The plan of a run at `tier`, not a draft, with no class.
pub(crate) fn plan(tier: &'static str) -> PlanDoc {
    let roles = match tier {
        "full" => vec!["bugs", "security", "design"],
        "lite" => vec!["combined"],
        "micro" => vec!["combined-micro"],
        _ => Vec::new(),
    };
    PlanDoc {
        tier,
        classes: Vec::new(),
        roles,
        draft: false,
        run_uid: this_run(1),
        budget: None,
    }
}

impl PlanDoc {
    /// A trust-path `full` run, with `meta`.
    pub(crate) fn trust_path(mut self) -> Self {
        self.classes = vec!["trust-path"];
        self.roles = vec!["bugs", "security", "design", "meta"];
        self
    }

    /// A draft.
    pub(crate) const fn draft(mut self) -> Self {
        self.draft = true;
        self
    }

    /// The document as `plan` wrote it.
    pub(crate) fn bytes(&self) -> Vec<u8> {
        let roles: &[&str] = if self.draft { &[] } else { &self.roles };
        let mut document = json!({
            "schema": "assay.run/1",
            "run_key": run_key(),
            "run_uid": self.run_uid,
            "repo": REPO,
            "pr": PR,
            "base_sha": "a".repeat(40),
            "head_sha": head(),
            "base_ref": "main",
            "tier": self.tier,
            "tier_reasons": [],
            "classes": self.classes,
            "roles": roles,
            "required_roles": self.roles,
            "shards": [],
            "plugins": [],
            "trust_paths_changed": [],
            "exec_surface_changed": [],
            "filtered_stats": { "files": 1, "lines": 30, "excluded": [] },
            "artifact_names": { "admit": "assay-admit-0123" },
            "draft": self.draft,
        });
        if let (Some(budget), Some(object)) = (&self.budget, document.as_object_mut()) {
            object.insert("budget".to_owned(), budget.clone());
        }
        serde_json::to_vec_pretty(&document).expect("a JSON value serialises")
    }
}

/// A clock standing at `now` that moves only when slept on, recording each sleep (TST-10).
#[derive(Debug)]
pub(crate) struct FakeClock {
    now: Cell<SystemTime>,
    pub(crate) sleeps: Vec<Duration>,
}

impl FakeClock {
    pub(crate) fn at_now() -> Self {
        Self {
            now: Cell::new(now_system()),
            sleeps: Vec::new(),
        }
    }
}

impl Clock for FakeClock {
    fn now(&self) -> SystemTime {
        self.now.get()
    }

    fn sleep(&mut self, duration: Duration) {
        self.sleeps.push(duration);
        self.now.set(self.now.get() + duration);
    }
}

/// Issue comments by id as GitHub shows them now; an id not held was deleted.
#[derive(Debug, Default)]
pub(crate) struct FakeComments {
    pub(crate) comments: BTreeMap<u64, IssueComment>,
    pub(crate) down: bool,
    pub(crate) asked: RefCell<Vec<CommentId>>,
}

impl CommandComments for FakeComments {
    type Error = FakeError;

    fn comment(&self, id: CommentId) -> Result<Option<IssueComment>, FakeError> {
        self.asked.borrow_mut().push(id);
        if self.down {
            return Err(FakeError);
        }
        Ok(self.comments.get(&id.0.get()).cloned())
    }
}

/// A sink keeping every event, on a clock standing at `NOW`.
#[derive(Debug, Default)]
pub(crate) struct RecordedEvents {
    pub(crate) events: Vec<Event>,
    /// The kind code the sink refuses, if any.
    pub(crate) refusing: Option<&'static str>,
}

impl RecordedEvents {
    /// Each event's kind code, in order.
    pub(crate) fn kinds(&self) -> Vec<&'static str> {
        self.events
            .iter()
            .map(|event| event.payload.kind().code())
            .collect()
    }
}

impl EventSink for RecordedEvents {
    type Error = FakeError;

    fn now(&self) -> SystemTime {
        now_system()
    }

    fn append(&mut self, event: &Event) -> Result<(), FakeError> {
        if self.refusing == Some(event.payload.kind().code()) {
            return Err(FakeError);
        }
        // The sink keeps its own copy of what it was given.
        self.events.push(event.clone());
        Ok(())
    }
}

/// What a failed `admit` returns over the world's ports.
pub(crate) type Failure =
    AdmitFailure<MemoryLedger, MemoryRefs, FakeBundles, FakeComments, RecordedEvents>;

/// Every port `admit` reads and writes, and what the caller hands it.
pub(crate) struct World {
    pub(crate) ledger: MemoryLedger,
    pub(crate) refs: MemoryRefs,
    pub(crate) bundles: FakeBundles,
    pub(crate) comments: FakeComments,
    pub(crate) clock: FakeClock,
    pub(crate) events: RecordedEvents,
    pub(crate) identities: Identities,
    pub(crate) carry: Carry,
    pub(crate) suppress: Option<String>,
    pub(crate) range: RangeIdentities,
    pub(crate) validator: ContractValidator,
}

impl World {
    /// An empty ledger, free slots, no previous run, and the owner and an agent listed.
    pub(crate) fn new() -> Self {
        Self {
            validator: ContractValidator::new().expect("the embedded schemas compile"),
            ledger: MemoryLedger::default(),
            refs: MemoryRefs::default(),
            bundles: FakeBundles::new(Stored::Missing),
            comments: FakeComments::default(),
            clock: FakeClock::at_now(),
            events: RecordedEvents::default(),
            identities: Identities {
                privileged: BTreeSet::from([UserId(nz(OWNER))]),
                agents: BTreeSet::from([UserId(nz(AGENT))]),
            },
            carry: Carry::FirstReview,
            suppress: None,
            range: authors_range(),
        }
    }

    /// A world whose ledger and refs cannot be reached.
    pub(crate) fn unreachable() -> Self {
        Self {
            ledger: MemoryLedger::default().down(),
            refs: MemoryRefs::default().down(),
            ..Self::new()
        }
    }

    /// Admits `plan` as attempt `run_uid` of this run.
    pub(crate) fn admit_as(&mut self, run_uid: RunUid, plan: &[u8]) -> Result<Admitted, Failure> {
        let request = AdmitRequest {
            run_uid,
            plan,
            carry: self.carry,
            identities: &self.identities,
            author: UserId(nz(AUTHOR)),
            range: &self.range,
            suppress: self.suppress.as_deref(),
        };
        let ports = AdmitPorts {
            ledger: &self.ledger,
            refs: &self.refs,
            bundles: &self.bundles,
            comments: &self.comments,
            clock: &mut self.clock,
            events: &mut self.events,
        };
        admit(ports, &request, &self.validator)
    }

    /// Admits `plan` as attempt 1 of this run.
    pub(crate) fn admit(&mut self, plan: &PlanDoc) -> Result<Admitted, Failure> {
        self.admit_as(this_run(1), &plan.bytes())
    }

    /// The ledger already holds `records`.
    pub(crate) fn spending(self, records: &[CostRecord]) -> Self {
        for record in records {
            append_cost(&self.ledger, record).expect("the fake ledger takes the record");
        }
        self
    }

    /// The PR's ledger state is `change` applied to the empty state.
    pub(crate) fn with_state(self, change: impl FnOnce(&mut PrState)) -> Self {
        let mut state = PrState::default();
        change(&mut state);
        update_state(&self.ledger, nz(REPO_ID), nz(PR), |_| Some(state.clone()))
            .expect("the fake ledger takes the state");
        self
    }

    /// The PR's ledger state now.
    pub(crate) fn state(&self) -> PrState {
        read_state(&self.ledger, nz(REPO_ID), nz(PR)).expect("the fake ledger reads")
    }

    /// The PR's deferral record now.
    pub(crate) fn deferral(&self) -> Option<DeferralRecord> {
        self.state().deferral
    }

    /// Slot `index` is held by run `run_id`'s lease expiring at `expires_at`.
    pub(crate) fn holding_slot(self, index: u8, run_id: u64, expires_at: &str) -> Self {
        let slot = Slot::new(index).expect("slots 0 and 1 exist");
        self.refs.holding(
            LeaseRef::slot(slot).as_str(),
            &crate::lease_fakes::lease_line(run_id, expires_at),
        );
        self
    }

    /// decide-only granted a retry of `role` on the PR's head.
    pub(crate) fn granting(self, role: Role) -> Self {
        let grant = RetryGrant {
            run_uid: RunUid {
                repo_id: nz(REPO_ID),
                run_id: nz(8),
                run_attempt: nz(1),
            },
            repo: REPO.to_owned(),
            pr: nz(PR),
            head_sha: head().parse().expect("40 hex digits"),
            role,
            at: at("2026-10-19T11:00:00.000Z"),
        };
        grant_retry(&self.refs, &grant).expect("the fake refs take the grant");
        self
    }

    /// The names of the refs held now.
    pub(crate) fn refs_held(&self) -> Vec<String> {
        self.refs.names()
    }
}

/// The retry grant ref of `role` on the PR's head.
pub(crate) fn grant_ref(role: Role) -> String {
    LeaseRef::retry_grant(
        nz(REPO_ID),
        nz(PR),
        &head().parse().expect("40 hex digits"),
        role,
    )
    .as_str()
    .to_owned()
}

/// A `lite` cost record of another run `run_id` on PR `pr`, of `kind`, for `micros`, at `when`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct OtherCost<'a> {
    pub(crate) run_id: u64,
    pub(crate) pr: u64,
    pub(crate) kind: CostKind,
    pub(crate) micros: u64,
    pub(crate) when: &'a str,
}

impl OtherCost<'_> {
    /// The record as the ledger holds it.
    pub(crate) fn record(self) -> CostRecord {
        CostRecord {
            run_uid: RunUid {
                repo_id: nz(REPO_ID),
                run_id: nz(self.run_id),
                run_attempt: nz(1),
            },
            run_key: format!("sha256:{}", "c".repeat(64))
                .parse()
                .expect("64 hex digits"),
            repo: REPO.to_owned(),
            pr: nz(self.pr),
            head_sha: "d".repeat(40).parse().expect("40 hex digits"),
            tier: Tier::Lite,
            roles: vec![RoleCost {
                role: CostRole::Role(Role::Combined),
                aeu: Aeu::from_micros(self.micros),
            }],
            aeu: Aeu::from_micros(self.micros),
            kind: self.kind,
            cancelled: false,
            at: at(self.when),
        }
    }
}

/// A range of one commit, authored and committed by the PR's author.
pub(crate) fn authors_range() -> RangeIdentities {
    let commit: CommitId = "a"
        .repeat(40)
        .parse()
        .expect("forty hex digits are a commit id");
    RangeIdentities::of(
        &[CommitIdentity {
            commit,
            authors: vec![Actor::User(UserId(nz(AUTHOR)))],
            committer: Actor::User(UserId(nz(AUTHOR))),
        }],
        std::collections::HashSet::new(),
    )
}
