//! `run_admit` (#60 ST-11, S1a) over fakes and a fixture PR planned by `run_plan`: the plan
//! artifact is authenticated by plan's `run_digest`, a run with no LLM stage reads nothing, and
//! a deferral or a spend writes the admitted documents and admit's outputs (03 §4a).

use std::assert_matches;
use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use assay_app::{
    ContractValidator, CostKind, CostRecord, CostRole, EvidencePointer, LedgerTime, PrState,
    PriorCarryError, Role, RoleCost, Sha256Digest, UserId, append_cost, update_state,
};
use assay_cli::outputs::{AdmitOutputs, Identity, PlanOutputs, Trigger};
use assay_cli::{
    AdmitJob, AdmitJobPorts, AdmitRunError, FirstAttempt, ForgePullRequest, OutDir, OutputName,
    PACK_FILE, PlanJob, PlanPorts, RUN_FILE, RunEnv, RunVariables, run_admit, run_plan,
};
use assay_domain::{Aeu, ContentHash, RunUid, Tier};
use assay_forge::git::TreeLimits;
use tempfile::TempDir;

use crate::admit_fakes::{
    Commits, Fetches, NOW_SECS, NoBundles, NoComments, StillClock, Untouched,
};
use crate::open_fakes::{MemoryLedger, MemoryRefs, Refused};
use crate::plan::{Counting, Pr, pr};

/// The PR's number and its author.
const PR: u64 = 55;
const AUTHOR: u64 = 7;

fn id(n: u64) -> Result<NonZeroU64, Box<dyn Error>> {
    Ok(NonZeroU64::new(n).ok_or("a positive id")?)
}

/// A planned PR changing the library, with plan's outputs and the document it wrote.
struct Planned {
    pr: Pr,
    _scratch: TempDir,
    out: PathBuf,
    run: FirstAttempt,
    identity: Identity,
    plan: PlanOutputs,
    document: Vec<u8>,
    validator: ContractValidator,
}

impl Planned {
    /// A ready (`draft` false) or draft PR, planned as `plan` would.
    fn new(draft: bool) -> Result<Self, Box<dyn Error>> {
        let pr = pr(&[], &[("src/lib.rs", "pub fn two() {}\n")])?;
        let run = RunEnv::from_vars(&RunVariables {
            run_id: Some(OsString::from("4242")),
            run_attempt: Some(OsString::from("1")),
            repository_id: Some(OsString::from("42")),
            repository: Some(OsString::from("Wet-Ink-Corporation/assay")),
        })?;
        let identity = Identity {
            pr: id(PR)?,
            head_sha: pr.head.as_str().parse()?,
            base_sha: pr.base.as_str().parse()?,
            base_ref: "main".to_owned(),
            draft,
            author: UserId(id(AUTHOR)?),
            sender: UserId(id(AUTHOR)?),
            trigger: Trigger::Opened,
            default_branch: "main".to_owned(),
        };
        let scratch = tempfile::tempdir()?;
        let root = fs::canonicalize(scratch.path())?;
        let plan_out = OutDir::new(&root.join("plan-out"))?;
        let validator = ContractValidator::new()?;
        let objects =
            ForgePullRequest::new(&pr.repo, TreeLimits::default(), Duration::from_secs(60));
        let plan = run_plan(
            PlanJob {
                run: &run,
                identity: identity.clone(),
                rest_files: assay_app::RestFiles {
                    changed_files: 1,
                    files: vec![assay_app::RestFile {
                        path: "src/lib.rs".to_owned(),
                        previous_path: None,
                    }],
                },
                prompt_bundle: ContentHash::of_files([]),
            },
            PlanPorts {
                objects: &objects,
                nonces: &mut Counting::default(),
                validator: &validator,
            },
            &plan_out,
        )?;
        let document = fs::read(plan_out.path().join(RUN_FILE))?;
        Ok(Self {
            pr,
            _scratch: scratch,
            out: root.join("admit-out"),
            run: run.first_attempt()?,
            identity,
            plan,
            document,
            validator,
        })
    }

    fn job<'a>(&'a self, document: &'a [u8]) -> AdmitJob<'a> {
        AdmitJob {
            run: &self.run,
            identity: &self.identity,
            plan: &self.plan,
            document,
            validator: &self.validator,
        }
    }

    /// The file `name` admit wrote.
    fn written(&self, name: &str) -> Result<Vec<u8>, Box<dyn Error>> {
        Ok(fs::read(self.out.join(name))?)
    }
}

/// The live-shaped ports: an in-memory ledger and refs, GitHub resolving the author, no
/// bundle, no comment.
#[derive(Default)]
struct World {
    ledger: MemoryLedger,
    refs: MemoryRefs,
    bundles: NoBundles,
    fetches: Fetches,
}

/// What `run_admit` gives over the fakes.
type Ran =
    Result<AdmitOutputs, AdmitRunError<Refused, Refused, Refused, Refused, Refused, Refused>>;

impl World {
    /// Admits `planned` over this world.
    fn admit(&self, planned: &Planned) -> Result<Ran, Box<dyn Error>> {
        let objects = ForgePullRequest::new(
            &planned.pr.repo,
            TreeLimits::default(),
            Duration::from_secs(60),
        );
        let forge = Commits {
            author: UserId(id(AUTHOR)?),
            rest: Untouched::default(),
        };
        let ports = AdmitJobPorts {
            fetch: &self.fetches,
            objects: &objects,
            forge: &forge,
            ledger: &self.ledger,
            refs: &self.refs,
            bundles: &self.bundles,
            comments: &NoComments,
            clock: &mut StillClock::new(),
        };
        Ok(run_admit(
            planned.job(&planned.document),
            ports,
            &OutDir::new(&planned.out)?,
        ))
    }
}

/// The names of `outputs`, in order.
fn names(outputs: &AdmitOutputs) -> Vec<OutputName> {
    outputs
        .outputs()
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

/// Output `name`'s value.
fn value(outputs: &AdmitOutputs, name: OutputName) -> Option<String> {
    outputs
        .outputs()
        .into_iter()
        .find_map(|(written, value)| (written == name).then_some(value))
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_draft_passes_the_plan_through_and_reads_nothing() -> Result<(), Box<dyn Error>> {
    let planned = Planned::new(true)?;
    let untouched = Untouched::default();
    let objects = ForgePullRequest::new(
        &planned.pr.repo,
        TreeLimits::default(),
        Duration::from_secs(60),
    );
    let ports = AdmitJobPorts {
        fetch: &untouched,
        objects: &objects,
        forge: &untouched,
        ledger: &untouched,
        refs: &untouched,
        bundles: &untouched,
        comments: &untouched,
        clock: &mut StillClock::new(),
    };

    let outputs = run_admit(
        planned.job(&planned.document),
        ports,
        &OutDir::new(&planned.out)?,
    )?;

    assert_eq!(untouched.calls(), Vec::<&str>::new(), "no port is read");
    assert_eq!(planned.written(RUN_FILE)?, planned.document);
    assert!(!planned.out.join(PACK_FILE).exists(), "no pack for a draft");
    let written: Vec<(&str, String)> = outputs
        .outputs()
        .into_iter()
        .map(|(name, value)| (name.as_str(), value))
        .collect();
    let tier = planned.plan.tier().code().to_owned();
    assert_eq!(
        written,
        [
            ("admission", "no-spend".to_owned()),
            ("reserved_aeu", "0".to_owned()),
            ("tier", tier),
            ("degrade", "none".to_owned()),
            ("run_digest", planned.plan.run_digest().to_string()),
            ("review", "false".to_owned()),
        ]
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_plan_artifact_that_is_not_plan_s_digest_takes_no_slot() -> Result<(), Box<dyn Error>> {
    let planned = Planned::new(false)?;
    let mut tampered = planned.document.clone();
    tampered.push(b'\n');
    let untouched = Untouched::default();
    let objects = ForgePullRequest::new(
        &planned.pr.repo,
        TreeLimits::default(),
        Duration::from_secs(60),
    );
    let ports = AdmitJobPorts {
        fetch: &untouched,
        objects: &objects,
        forge: &untouched,
        ledger: &untouched,
        refs: &untouched,
        bundles: &untouched,
        comments: &untouched,
        clock: &mut StillClock::new(),
    };

    let refused = run_admit(planned.job(&tampered), ports, &OutDir::new(&planned.out)?);

    assert_matches!(refused, Err(AdmitRunError::PlanDigest));
    assert_eq!(untouched.calls(), Vec::<&str>::new(), "nothing is read");
    assert!(!planned.out.exists(), "nothing is written");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_spend_holds_a_slot_and_writes_the_admitted_documents() -> Result<(), Box<dyn Error>> {
    let planned = Planned::new(false)?;
    let world = World::default();

    let outputs = world.admit(&planned)??;

    assert_eq!(
        names(&outputs),
        [
            OutputName::Admission,
            OutputName::Slot,
            OutputName::LeaseCommit,
            OutputName::ReservedAeu,
            OutputName::Tier,
            OutputName::Degrade,
            OutputName::RunDigest,
            OutputName::PackDigest,
            OutputName::Review,
        ]
    );
    assert_eq!(
        value(&outputs, OutputName::Admission).as_deref(),
        Some("spend")
    );
    assert_eq!(value(&outputs, OutputName::Review).as_deref(), Some("true"));
    let held = outputs.held().ok_or("a spend holds a slot")?;
    assert_eq!(
        world.refs.names(),
        [format!("refs/heads/slot-{}", held.slot)]
    );
    let run_json = planned.written(RUN_FILE)?;
    let pack = planned.written(PACK_FILE)?;
    assert_eq!(outputs.run_digest(), &Sha256Digest::of(&run_json));
    assert_eq!(outputs.pack_digest(), Some(&Sha256Digest::of(&pack)));
    assert_ne!(
        run_json, planned.document,
        "the admitted document carries its budget"
    );
    let fetched: Vec<String> = world
        .fetches
        .asked
        .borrow()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        fetched,
        [planned.pr.base.to_string(), planned.pr.head.to_string()]
    );
    Ok(())
}

/// A `reserved` record of another run on the PR, written ten minutes ago.
fn admission(run_id: u64) -> Result<CostRecord, Box<dyn Error>> {
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS - 600);
    Ok(CostRecord {
        run_uid: RunUid {
            repo_id: id(42)?,
            run_id: id(run_id)?,
            run_attempt: NonZeroU64::MIN,
        },
        run_key: format!("sha256:{}", "c".repeat(64)).parse()?,
        repo: "Wet-Ink-Corporation/assay".to_owned(),
        pr: id(PR)?,
        head_sha: "d".repeat(40).parse()?,
        tier: Tier::Lite,
        roles: vec![RoleCost {
            role: CostRole::Role(Role::Combined),
            aeu: Aeu::from_micros(250_000),
        }],
        aeu: Aeu::from_micros(250_000),
        kind: CostKind::Reserved,
        cancelled: false,
        at: LedgerTime::from_system_time(at)?,
    })
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_deferral_reports_its_code_and_holds_no_slot() -> Result<(), Box<dyn Error>> {
    let planned = Planned::new(false)?;
    let world = World::default();
    for run_id in [71, 72, 73] {
        append_cost(&world.ledger, &admission(run_id)?)?;
    }

    let outputs = world.admit(&planned)??;

    assert_eq!(
        value(&outputs, OutputName::Admission).as_deref(),
        Some("deferred")
    );
    assert_eq!(
        value(&outputs, OutputName::Deferral).as_deref(),
        Some("budget-deferred")
    );
    assert_eq!(value(&outputs, OutputName::Slot), None);
    assert_eq!(
        value(&outputs, OutputName::Review).as_deref(),
        Some("false")
    );
    assert_eq!(world.refs.names(), Vec::<String>::new());
    assert_eq!(
        outputs.run_digest(),
        &Sha256Digest::of(&planned.written(RUN_FILE)?)
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn the_previous_head_is_read_from_the_bundle_the_ledger_points_at() -> Result<(), Box<dyn Error>> {
    let planned = Planned::new(false)?;
    let world = World::default();
    let pointer = EvidencePointer {
        run_id: id(4000)?,
        run_attempt: NonZeroU64::MIN,
        sha256: format!("sha256:{}", "e".repeat(64)).parse()?,
    };
    let state = PrState {
        evidence: Some(pointer),
        ..PrState::default()
    };
    update_state(&world.ledger, id(42)?, id(PR)?, |_| Some(state.clone()))?;

    let refused = world.admit(&planned)?;

    assert_matches!(
        refused,
        Err(AdmitRunError::PriorHead(PriorCarryError::Missing))
    );
    assert_eq!(
        world.bundles.asked.borrow().as_slice(),
        [RunUid {
            repo_id: id(42)?,
            run_id: id(4000)?,
            run_attempt: NonZeroU64::MIN,
        }]
    );
    assert_eq!(world.refs.names(), Vec::<String>::new(), "no slot is taken");
    Ok(())
}
