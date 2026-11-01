//! `run_redecide` (#61 DO-11) over fakes and a real evidence bundle: the fixture PR planned by
//! `run_plan` and decided by the pipeline's `run_decide` with an opener push record, so the
//! bundle decide-only reads is one assay wrote. `branches` holds the branches of the flow the
//! review asked for (#61 DO-11 review round 1).

#[cfg(test)] // lets clippy's allow-*-in-tests cover helpers, not only #[test] fns
mod branches;

use std::cell::RefCell;
use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::num::NonZeroU64;
use std::path::PathBuf;

use assay_app::{
    Absence, ActorCounter, BUNDLE_FILE, CommandRejected, CommentAction, CommentEvent, CommentId,
    CommitId, DECISION_FILE, DecisionPointer, DeferralRecord, EvidencePointer, Forgery, Ineligible,
    IssueComment, MAX_COMMANDS_PER_ACTOR, PrState, PublicationRegister, PushDraft, PushKind,
    RedecideRefusal, Reply, Sha256Digest, StoredBundle, Timestamp, append_push, read_commands,
    read_state, update_state,
};
use assay_cli::outputs::{DecideOnlyGuardOutputs, DecideOnlyPrs, DecideTrigger, RedecideOutputs};
use assay_cli::{
    FirstAttempt, OutDir, PayloadMismatch, RedecideJob, RedecideJobPorts, RedecideRunError,
    RedecideState, Redecision, RunEnv, RunVariables, STATE_FILE, run_redecide,
};
use assay_domain::{Decision, ReasonCode, Rubric};
use assay_forge::github::{
    CommentPayload, DecideOnlyEvent, HeadRepository, Label, LabelAction, LabelPayload, Sender,
    SenderKind,
};

use crate::decide_fakes::Reviewless;
use crate::decide_run::Planned;
use crate::open_fakes::{MemoryLedger, Refused};
use crate::plan::plugins_toml;
use crate::redecide_fakes::{Hub, Log, LoggedLedger, pipeline_run, user};

/// The fixture PR, its author, the repository, the pipeline's run and decide-only's.
const PR: u64 = 55;
const AUTHOR: u64 = 7;
const REPO: u64 = 42;
const PIPELINE_RUN: u64 = 4242;
const DECIDE_ONLY_RUN: &str = "5000";

/// The base's eligibility lists: owner 1001 and approver 1002.
const OWNERS: &str = "owners = [{ id = 1001, login = \"owner\" }]\n\
                      approvers = [{ id = 1002, login = \"approver\" }]\nmaintainers = []\n";
const OWNER: u64 = 1001;
/// Someone in no list.
const OUTSIDER: u64 = 99;

/// When the head was opened, and comments an hour before and after.
const PUSHED: &str = "2026-11-01T10:00:00.000Z";
const LATER: &str = "2026-11-01T11:00:00.000Z";
const EARLIER: &str = "2026-11-01T09:00:00.000Z";
/// When decide-only runs.
const NOW: &str = "2026-11-01T12:00:00.000Z";

/// The comment the tests post.
const COMMENT: u64 = 9001;

/// The ledger's state file of the PR, and its command records.
const STATE_PATH: &str = "state/42/55.json";
const COMMANDS_PATH: &str = "commands/42/55.jsonl";

/// Another head than the fixture's.
const OTHER_HEAD: &str = "9999999999999999999999999999999999999999";

fn id(n: u64) -> Result<NonZeroU64, Box<dyn Error>> {
    Ok(NonZeroU64::new(n).ok_or("a positive id")?)
}

/// A fixture PR decided by the pipeline, its bundle, and the ledger holding its push record,
/// its evidence pointer and a publication register of the head, written by the evidence's
/// pipeline run under a key no re-decision gives.
struct Scene {
    planned: Planned,
    files: MemoryLedger,
    bundle: Vec<u8>,
    decision: Decision,
    run: FirstAttempt,
    log: Log,
    outs: RefCell<u32>,
}

impl Scene {
    fn new() -> Result<Self, Box<dyn Error>> {
        // `main` is protected, so the decision fails only for what an override may neutralise
        // (R3-03: `base-not-protected` never is).
        let plugins =
            plugins_toml(true).replacen("[assay]\n", "[assay]\nprotected_bases = [\"main\"]\n", 1);
        let planned =
            Planned::with_base(&[(".assay/owners", OWNERS), (".assay/plugins.toml", &plugins)])?;
        let files = MemoryLedger::default();
        let head = planned.needs.identity.head_sha.clone();
        append_push(
            &files,
            &PushDraft {
                run_uid: planned.run.run_env().run_uid,
                pr: id(PR)?,
                kind: PushKind::Opener,
                after: head,
                sender_id: user(AUTHOR)?,
                at: PUSHED.parse()?,
            },
        )?;
        let pipeline = planned.decide_over(&planned.unmoved(), &files)?;
        pipeline.map_err(|error| format!("the pipeline's decide failed: {error}"))?;
        let bundle = fs::read(planned.out().join(BUNDLE_FILE))?;
        let value: serde_json::Value = serde_json::from_slice(&bundle)?;
        let decision: Decision = serde_json::from_value(value["decision"].clone())?;
        let pointer = EvidencePointer {
            run_id: planned.run.run_env().run_uid.run_id,
            run_attempt: NonZeroU64::MIN,
            sha256: Sha256Digest::of(&bundle),
        };
        assert_eq!(pointer.run_id.get(), PIPELINE_RUN);
        let register = PublicationRegister {
            head_sha: planned.needs.identity.head_sha.clone(),
            key: Sha256Digest::of(b"an older decision"),
            run_id: pointer.run_id,
        };
        update_state(&files, id(REPO)?, id(PR)?, |state| {
            // update_state shows a borrowed state and takes an owned next one.
            let mut next = state.clone();
            next.evidence = Some(pointer.clone());
            next.published = Some(register.clone());
            Some(next)
        })?;
        let run = RunEnv::from_vars(&RunVariables {
            run_id: Some(OsString::from(DECIDE_ONLY_RUN)),
            run_attempt: Some(OsString::from("1")),
            repository_id: Some(OsString::from(REPO.to_string())),
            repository: Some(OsString::from("Wet-Ink-Corporation/assay")),
        })?
        .first_attempt()?;
        Ok(Self {
            planned,
            files,
            bundle,
            decision,
            run,
            log: Log::default(),
            outs: RefCell::new(0),
        })
    }

    fn head(&self) -> CommitId {
        // Each caller owns its copy of the head.
        self.planned.needs.identity.head_sha.clone()
    }

    /// Sets the PR's publication register to `head` under `key`, written by run `run_id`.
    fn register(
        &self,
        head: &CommitId,
        key: &Sha256Digest,
        run_id: NonZeroU64,
    ) -> Result<(), Box<dyn Error>> {
        self.edit_state(|state| {
            // Each edit owns its register.
            state.published = Some(PublicationRegister {
                head_sha: head.clone(),
                key: key.clone(),
                run_id,
            });
        })
    }

    /// GitHub with the PR unmoved, the base's owners at the tip, and the bundle stored.
    fn hub(&self) -> Result<Hub<'_>, Box<dyn Error>> {
        let mut hub = Hub {
            current: self.planned.unmoved().current,
            owners: Some(OWNERS.as_bytes().to_vec()),
            run: pipeline_run(self.planned.run.run_env().run_uid),
            stored: StoredBundle::Found(self.bundle.clone()),
            replies: RefCell::default(),
            comments: Vec::new(),
            forge: Reviewless::by(user(AUTHOR)?),
            log: &self.log,
        };
        hub.current.base_ref = "main".to_owned();
        Ok(hub)
    }

    /// Runs decide-only for `event` (of `trigger`) over `hub`, into a fresh `--out`.
    fn redecide(
        &self,
        hub: &Hub<'_>,
        event: &DecideOnlyEvent,
        trigger: DecideTrigger,
    ) -> Result<Redecided, Box<dyn Error>> {
        self.redecide_over(hub, event, (trigger, self.as_fixture()))
    }

    /// Like [`Scene::redecide`], with `over` in place of the fixture's rubric and ledger.
    fn redecide_over(
        &self,
        hub: &Hub<'_>,
        event: &DecideOnlyEvent,
        (trigger, over): (DecideTrigger, Over<'_>),
    ) -> Result<Redecided, Box<dyn Error>> {
        let guard = DecideOnlyGuardOutputs::new(DecideOnlyPrs::one(id(PR)?), trigger);
        let (ran, out) = self.attempt_over(hub, event, (&guard, over))?;
        let Redecision { outputs, forgery } =
            ran.map_err(|error| format!("run_redecide failed: {error}"))?;
        Ok(Redecided {
            outputs,
            forgery,
            out,
        })
    }

    /// The fixture's rubric and a ledger that refuses nothing.
    const fn as_fixture(&self) -> Over<'_> {
        Over {
            rubric: &self.planned.rubric,
            refuse: None,
        }
    }

    /// Runs decide-only for `event` after `guard` over `hub`, into a fresh `--out`, which is
    /// returned beside what it gave.
    fn attempt(
        &self,
        hub: &Hub<'_>,
        event: &DecideOnlyEvent,
        guard: &DecideOnlyGuardOutputs,
    ) -> Result<(Attempted, PathBuf), Box<dyn Error>> {
        self.attempt_over(hub, event, (guard, self.as_fixture()))
    }

    /// Like [`Scene::attempt`], with `over` in place of the fixture's rubric and ledger.
    fn attempt_over(
        &self,
        hub: &Hub<'_>,
        event: &DecideOnlyEvent,
        (guard, over): (&DecideOnlyGuardOutputs, Over<'_>),
    ) -> Result<(Attempted, PathBuf), Box<dyn Error>> {
        let out = {
            let mut outs = self.outs.borrow_mut();
            *outs = outs.saturating_add(1);
            self.planned.root.join(format!("redecide-out-{outs}"))
        };
        let ledger = LoggedLedger {
            files: &self.files,
            log: &self.log,
            refuse: over.refuse,
        };
        let ran = run_redecide(
            &RedecideJob {
                run: &self.run,
                guard,
                pr: id(PR)?,
                event,
                rubric: over.rubric,
                at: NOW.parse()?,
                nonce: format!("{:032x}", 0x0123_4567_89ab_cdef_0123_4567_89ab_cdef_u128)
                    .parse()?,
            },
            &RedecideJobPorts {
                app: hub,
                run: hub,
                ledger: &ledger,
            },
            &OutDir::new(&out)?,
        );
        Ok((ran, out))
    }

    /// The PR's ledger state now.
    fn state(&self) -> Result<PrState, Box<dyn Error>> {
        Ok(read_state(&self.files, id(REPO)?, id(PR)?)?)
    }

    /// Changes the PR's ledger state with `edit`.
    fn edit_state(&self, edit: impl Fn(&mut PrState)) -> Result<(), Box<dyn Error>> {
        update_state(&self.files, id(REPO)?, id(PR)?, |state| {
            // update_state shows a borrowed state and takes an owned next one.
            let mut next = state.clone();
            edit(&mut next);
            Some(next)
        })?;
        Ok(())
    }

    /// The ledger paths written.
    fn written(&self) -> Vec<String> {
        self.log
            .borrow()
            .iter()
            .filter_map(|call| call.strip_prefix("write ").map(str::to_owned))
            .collect()
    }
}

/// What `run_redecide` gives over the fakes.
type Attempted = Result<Redecision, RedecideRunError<Refused, Refused, Refused, Refused>>;

/// What a run differs in from the fixture's: the rubric it decides under, and the ledger path
/// prefix whose writes fail.
#[derive(Clone, Copy)]
struct Over<'r> {
    rubric: &'r Rubric,
    refuse: Option<&'static str>,
}

/// What one decide-only run left.
struct Redecided {
    outputs: RedecideOutputs,
    forgery: Option<Forgery>,
    out: PathBuf,
}

impl Redecided {
    fn state(&self) -> Result<RedecideState, Box<dyn Error>> {
        Ok(RedecideState::read(&fs::read(self.out.join(STATE_FILE))?)?)
    }

    fn has_bundle(&self) -> bool {
        self.out.join(DECISION_FILE).exists()
    }

    /// The decision bundle written, as JSON.
    fn bundle(&self) -> Result<serde_json::Value, Box<dyn Error>> {
        Ok(serde_json::from_slice(&fs::read(
            self.out.join(DECISION_FILE),
        )?)?)
    }

    /// Whether the bundle's decision applied an override.
    fn overridden(&self) -> Result<bool, Box<dyn Error>> {
        let decision: Decision = serde_json::from_value(self.bundle()?["decision"].clone())?;
        Ok(decision
            .reasons
            .iter()
            .any(|reason| reason.code == ReasonCode::Override))
    }
}

/// An `issue_comment` `created` on the PR by `actor`, with `body`, posted at `at`.
fn comment(actor: u64, body: &str, at: &str) -> Result<DecideOnlyEvent, Box<dyn Error>> {
    comment_via(actor, body, at, false)
}

/// Like [`comment`], posted through an app when `via_app`.
fn comment_via(
    actor: u64,
    body: &str,
    at: &str,
    via_app: bool,
) -> Result<DecideOnlyEvent, Box<dyn Error>> {
    let at: Timestamp = at.parse()?;
    Ok(DecideOnlyEvent::Comment(CommentPayload {
        repository_id: id(REPO)?,
        pr: Some(id(PR)?),
        sender: Sender {
            id: user(actor)?,
            kind: SenderKind::User,
        },
        event: CommentEvent {
            action: CommentAction::Created,
            comment: IssueComment {
                id: CommentId(id(COMMENT)?),
                user: Some(user(actor)?),
                created_at: at,
                updated_at: at,
                body: body.to_owned(),
                via_app,
            },
        },
    }))
}

/// `/assay recheck` by the owner, after the push.
fn recheck() -> Result<DecideOnlyEvent, Box<dyn Error>> {
    comment(OWNER, "/assay recheck", LATER)
}

/// An `assay:*` label added to the PR by the author.
fn label(scene: &Scene) -> Result<DecideOnlyEvent, Box<dyn Error>> {
    Ok(DecideOnlyEvent::Label(LabelPayload {
        action: LabelAction::Labeled,
        label: Label::Name(
            "assay:needs-owner"
                .to_owned()
                .try_into()
                .map_err(|_over_cap| "a label name")?,
        ),
        number: id(PR)?,
        head_sha: scene.head(),
        head_repository: HeadRepository::Base,
        base_ref: "main".to_owned(),
        sender: Sender {
            id: user(AUTHOR)?,
            kind: SenderKind::User,
        },
        repository_id: id(REPO)?,
    }))
}

/// The replies posted, each to the test's comment on the PR.
fn replied(hub: &Hub<'_>) -> Result<Vec<Reply>, Box<dyn Error>> {
    let pr = id(PR)?;
    let to = CommentId(id(COMMENT)?);
    Ok(hub
        .replies
        .borrow()
        .iter()
        .map(|&(on, answered, reply)| {
            assert_eq!(
                (on, answered),
                (pr, to),
                "a reply to the comment, on the PR"
            );
            reply
        })
        .collect())
}

/// The override line for the evidence decision's reasons.
fn override_line(scene: &Scene) -> String {
    format!(
        "/assay override all {} the owner accepts the risk",
        scene.decision.reasons_digest()
    )
}

/// Asserts `ran` is silent with no bundle.
fn assert_silent(ran: &Redecided) -> Result<(), Box<dyn Error>> {
    assert_eq!(ran.outputs, RedecideOutputs::NoBundle);
    assert_eq!(ran.state()?, RedecideState::Silent);
    assert!(!ran.has_bundle(), "no decision bundle");
    Ok(())
}

/// Asserts that each `(actor, body, at, via_app)` comment is answered `expected`, silently.
fn assert_rejected(
    scene: &Scene,
    cases: &[(u64, &str, &str, bool, Reply)],
) -> Result<(), Box<dyn Error>> {
    for &(actor, body, at, via_app, expected) in cases {
        let hub = scene.hub()?;

        let ran = scene.redecide(
            &hub,
            &comment_via(actor, body, at, via_app)?,
            DecideTrigger::Comment,
        )?;

        assert_eq!(replied(&hub)?, [expected], "{body}");
        assert_silent(&ran)?;
    }
    assert!(
        read_commands(&scene.files, id(REPO)?, id(PR)?)?.is_empty(),
        "no rejected command is recorded"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_pr_guard_did_not_schedule_is_refused_before_any_call() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let hub = scene.hub()?;
    let guard = DecideOnlyGuardOutputs::new(DecideOnlyPrs::one(id(56)?), DecideTrigger::Label);

    let (ran, out) = scene.attempt(&hub, &label(&scene)?, &guard)?;

    assert!(
        matches!(ran, Err(RedecideRunError::NotScheduled { pr }) if pr.get() == PR),
        "{ran:?}"
    );
    assert!(scene.log.borrow().is_empty(), "nothing read or written");
    assert!(!out.exists(), "no --out");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn an_eligible_override_is_recorded_with_the_live_head_and_applied() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    assert_eq!(scene.decision.conclusion, assay_domain::Conclusion::Failure);
    let hub = scene.hub()?;
    let line = override_line(&scene);

    let ran = scene.redecide(&hub, &comment(OWNER, &line, LATER)?, DecideTrigger::Comment)?;

    let records = read_commands(&scene.files, id(REPO)?, id(PR)?)?;
    let [record] = records.as_slice() else {
        return Err(format!("one command record, got {}", records.len()).into());
    };
    assert_eq!(record.head_sha, scene.head());
    assert_eq!(record.command, line);
    assert_eq!(record.actor_id, user(OWNER)?);
    assert_eq!(record.comment_id, CommentId(id(COMMENT)?));
    let digest = Sha256Digest::of(&fs::read(ran.out.join(DECISION_FILE))?);
    assert_eq!(
        ran.outputs,
        RedecideOutputs::Bundle {
            digest: digest.clone(),
            artifact: "assay-decision-55-0123456789abcdef0123456789abcdef".to_owned(),
        }
    );
    assert_eq!(ran.state()?, RedecideState::Decided { digest });
    assert!(ran.overridden()?, "the override applies");
    let bundle = ran.bundle()?;
    assert_eq!(bundle["inputs"]["override_record"]["by"], "owner");
    assert_eq!(bundle["trigger"]["event"], "comment");
    assert!(
        replied(&hub)?.is_empty(),
        "an accepted command gets no reply"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn an_unknown_malformed_or_retry_command_is_answered_and_not_recorded() -> Result<(), Box<dyn Error>>
{
    let scene = Scene::new()?;

    assert_rejected(
        &scene,
        &[
            (
                OWNER,
                "/assay frobnicate",
                LATER,
                false,
                Reply::Rejected(CommandRejected::Unknown),
            ),
            (
                OWNER,
                "/assay override all not-a-digest why",
                LATER,
                false,
                Reply::Rejected(CommandRejected::Malformed),
            ),
            (
                OWNER,
                "/assay retry security",
                LATER,
                false,
                Reply::RetryUnavailable,
            ),
        ],
    )
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn an_ineligible_early_or_app_posted_command_is_answered_and_not_recorded()
-> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let line = override_line(&scene);
    let ineligible = CommandRejected::Ineligible(Ineligible::NotPrivileged);

    assert_rejected(
        &scene,
        &[
            (OUTSIDER, &line, LATER, false, Reply::Rejected(ineligible)),
            (
                OWNER,
                &line,
                EARLIER,
                false,
                Reply::Rejected(CommandRejected::BeforePushClock),
            ),
            (
                OWNER,
                &line,
                LATER,
                true,
                Reply::Rejected(CommandRejected::ViaApp),
            ),
        ],
    )
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn an_actor_past_the_limit_is_answered_and_still_counted() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let outsider = user(OUTSIDER)?;
    scene.edit_state(|state| {
        state.actor_counters.push(ActorCounter {
            actor_id: outsider,
            count: MAX_COMMANDS_PER_ACTOR,
        });
    })?;
    let hub = scene.hub()?;

    let ran = scene.redecide(
        &hub,
        &comment(OUTSIDER, "/assay recheck", LATER)?,
        DecideTrigger::Comment,
    )?;

    assert_eq!(replied(&hub)?, [Reply::RateLimited]);
    assert_silent(&ran)?;
    assert_eq!(
        scene.state()?.actor_counters,
        [ActorCounter {
            actor_id: outsider,
            count: MAX_COMMANDS_PER_ACTOR.saturating_add(1),
        }]
    );
    assert!(
        !scene.log.borrow().iter().any(|call| call == "fetch bundle"),
        "no evidence read"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn unusable_evidence_is_a_refusal_with_no_bundle() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let mut tampered = scene.bundle.clone();
    tampered.push(b'\n');
    let cases = [
        (StoredBundle::Found(tampered), RedecideRefusal::Forged),
        (
            StoredBundle::Expired,
            RedecideRefusal::Absent(Absence::Expired),
        ),
        (
            StoredBundle::Missing,
            RedecideRefusal::Absent(Absence::Missing),
        ),
    ];
    for (stored, expected) in cases {
        let mut hub = scene.hub()?;
        hub.stored = stored;

        let ran = scene.redecide(&hub, &recheck()?, DecideTrigger::Comment)?;

        assert_eq!(ran.state()?, RedecideState::Refused(expected));
        assert_eq!(ran.outputs, RedecideOutputs::NoBundle);
        assert!(!ran.has_bundle());
        assert!(
            replied(&hub)?.is_empty(),
            "a refusal is published, not replied"
        );
    }
    assert_eq!(
        RedecideState::Refused(RedecideRefusal::Forged).json(),
        br#"{"reason":{"code":"gate-error","detail":"bundle-digest"},"verdict":"refused"}"#
    );
    assert_eq!(
        RedecideState::Refused(RedecideRefusal::Absent(Absence::Expired)).json(),
        br#"{"reason":{"code":"bundle-expired","detail":"expired"},"verdict":"refused"}"#
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn no_register_of_the_head_is_answered_and_silent() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let registers = [
        None,
        Some(PublicationRegister {
            head_sha: OTHER_HEAD.parse()?,
            key: Sha256Digest::of(b"another head's decision"),
            run_id: id(PIPELINE_RUN)?,
        }),
    ];
    for register in registers {
        // Each edit owns its register.
        scene.edit_state(|state| state.published = register.clone())?;
        let hub = scene.hub()?;

        let ran = scene.redecide(&hub, &recheck()?, DecideTrigger::Comment)?;

        assert_eq!(replied(&hub)?, [Reply::NoEvidence]);
        assert_silent(&ran)?;
    }
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn no_evidence_pointer_is_answered_and_silent() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    scene.edit_state(|state| state.evidence = None)?;
    let hub = scene.hub()?;

    let ran = scene.redecide(&hub, &recheck()?, DecideTrigger::Comment)?;

    assert_eq!(replied(&hub)?, [Reply::NoEvidence]);
    assert_silent(&ran)?;
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_register_of_another_run_than_the_pointers_is_not_overwritten() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    // A newer pipeline run refused the same head, after the evidence run (wi-b70b90).
    scene.register(
        &scene.head(),
        &Sha256Digest::of(b"a newer refusal"),
        id(4300)?,
    )?;
    let hub = scene.hub()?;

    let ran = scene.redecide(&hub, &label(&scene)?, DecideTrigger::Label)?;

    assert_silent(&ran)?;
    assert!(
        !scene.log.borrow().iter().any(|call| call == "workflow run"),
        "no evidence read"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_register_of_the_decision_pointers_run_is_decided_over() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let earlier_decision = id(4800)?;
    scene.edit_state(|state| {
        state.decision = Some(DecisionPointer {
            run_id: earlier_decision,
            sha256: Sha256Digest::of(b"an earlier decision bundle"),
        });
    })?;
    scene.register(
        &scene.head(),
        &Sha256Digest::of(b"an earlier re-decision"),
        earlier_decision,
    )?;
    let hub = scene.hub()?;

    let ran = scene.redecide(&hub, &label(&scene)?, DecideTrigger::Label)?;

    assert!(ran.has_bundle());
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_moved_head_is_answered_and_silent() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let moved: CommitId = OTHER_HEAD.parse()?;
    let mut hub = scene.hub()?;
    scene.register(
        &moved,
        &Sha256Digest::of(b"the new head's decision"),
        id(PIPELINE_RUN)?,
    )?;
    hub.current.head_sha = moved;

    let ran = scene.redecide(&hub, &recheck()?, DecideTrigger::Comment)?;

    assert_eq!(replied(&hub)?, [Reply::HeadMoved]);
    assert_silent(&ran)?;
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn an_unchanged_key_is_silent_with_no_bundle() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let hub = scene.hub()?;
    let first = scene.redecide(&hub, &label(&scene)?, DecideTrigger::Label)?;
    let key: Sha256Digest = first.bundle()?["inputs"]["decision_key"]
        .as_str()
        .ok_or("a decision key")?
        .parse()?;
    scene.register(&scene.head(), &key, id(PIPELINE_RUN)?)?;

    let again = scene.redecide(&hub, &label(&scene)?, DecideTrigger::Label)?;

    assert_silent(&again)?;
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_deferral_of_the_head_is_flagged_and_nothing_else_is_written() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let deferral = DeferralRecord {
        repo: "Wet-Ink-Corporation/assay".to_owned(),
        pr: id(PR)?,
        head: scene.head(),
        count: 2,
        resets_at: None,
        flag: false,
    };
    // The state stores its own copy; the test keeps one to compare.
    scene.edit_state(|state| state.deferral = Some(deferral.clone()))?;
    let before = scene.state()?;
    scene.log.borrow_mut().clear();
    let hub = scene.hub()?;

    let ran = scene.redecide(
        &hub,
        &comment(OWNER, &override_line(&scene), LATER)?,
        DecideTrigger::Comment,
    )?;

    assert!(ran.has_bundle());
    assert_eq!(scene.written(), [STATE_PATH, COMMANDS_PATH, STATE_PATH]);
    let after = scene.state()?;
    assert_eq!(
        after.deferral,
        Some(DeferralRecord {
            flag: true,
            ..deferral
        })
    );
    assert_eq!(
        (after.evidence, after.decision, after.evidence_records),
        (before.evidence, before.decision, before.evidence_records),
        "only the counter and the flag change in the state"
    );
    assert_eq!(ran.bundle()?["inputs"]["pr"]["requeues"], 2);
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn the_command_is_counted_before_the_evidence_pointer_is_read() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let hub = scene.hub()?;

    scene.redecide(
        &hub,
        &comment(OWNER, &override_line(&scene), LATER)?,
        DecideTrigger::Comment,
    )?;

    let log = scene.log.borrow();
    let read_state = format!("read {STATE_PATH}");
    let write_state = format!("write {STATE_PATH}");
    let expected = [
        "current",
        "lists at",
        read_state.as_str(),
        write_state.as_str(),
        read_state.as_str(),
        "default branch",
        "workflow run",
        "fetch bundle",
    ]
    .map(str::to_owned);
    assert_eq!(log.get(..8), Some(expected.as_slice()), "{log:?}");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_bots_comment_decides_nothing_and_calls_no_port() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let hub = scene.hub()?;
    let mut event = recheck()?;
    if let DecideOnlyEvent::Comment(payload) = &mut event {
        payload.sender.kind = SenderKind::Bot;
    }

    let ran = scene.redecide(&hub, &event, DecideTrigger::Comment)?;

    assert_silent(&ran)?;
    assert!(scene.log.borrow().is_empty(), "{:?}", scene.log.borrow());
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_payload_for_another_pr_or_event_is_refused() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let hub = scene.hub()?;
    let comment_guard =
        DecideOnlyGuardOutputs::new(DecideOnlyPrs::one(id(PR)?), DecideTrigger::Comment);
    let mut other_pr = recheck()?;
    if let DecideOnlyEvent::Comment(payload) = &mut other_pr {
        payload.pr = Some(id(56)?);
    }
    let a_label = label(&scene)?;
    let cases = [
        (&other_pr, PayloadMismatch::PullRequest),
        (&a_label, PayloadMismatch::Trigger),
    ];
    for (event, expected) in cases {
        let (ran, _) = scene.attempt(&hub, event, &comment_guard)?;

        assert!(
            matches!(ran, Err(RedecideRunError::Payload(mismatch)) if mismatch == expected),
            "{ran:?}"
        );
    }
    assert!(scene.log.borrow().is_empty());
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_command_already_recorded_for_its_comment_is_accepted_once() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let event = comment(OWNER, &override_line(&scene), LATER)?;
    let DecideOnlyEvent::Comment(payload) = &event else {
        return Err("a comment".into());
    };
    let mut hub = scene.hub()?;
    // The re-fetch finds the comment as posted, so its record stands.
    hub.comments = vec![payload.event.comment.clone()];
    scene.redecide(&hub, &event, DecideTrigger::Comment)?;

    let again = scene.redecide(&hub, &event, DecideTrigger::Comment)?;

    assert_eq!(read_commands(&scene.files, id(REPO)?, id(PR)?)?.len(), 1);
    assert!(again.has_bundle());
    assert!(again.overridden()?, "the recorded override still applies");
    assert!(replied(&hub)?.is_empty());
    let bundle = again.bundle()?;
    let commands = bundle["github"]["commands"]
        .as_array()
        .ok_or("the snapshot's commands")?;
    let [command] = commands.as_slice() else {
        return Err(format!("one command in the snapshot, got {}", commands.len()).into());
    };
    assert_eq!(command["comment_id"], COMMENT, "the comment's own record");
    Ok(())
}
