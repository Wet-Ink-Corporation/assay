//! A thread-safe scripted model runner for the specialists' fan-out: it answers each role from
//! its own script, records every call and the order calls start and end in, counts the calls
//! in flight, and can hold a call until other calls have started. Every hold is bounded, so a
//! wrong fan-out fails an assertion instead of hanging.

use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use assay_app::{
    BundleFiles, InitReadError, ModelCall, ModelOutcome, ModelResult, ModelRunner, PromptBundle,
    Role, SpecialistsRequest, Structured,
};
use assay_domain::{Aeu, RequiredRole};
use serde_json::{Value, json};

/// The longest a held call waits for the calls it waits on; a correct fan-out never needs it.
const HOLD_LIMIT: Duration = Duration::from_secs(30);

/// The context pack every test sends.
pub(crate) const PACK: &str = "PACK";

/// The findings schema every test passes, as text.
pub(crate) const SCHEMA: &str = r#"{"type":"object"}"#;

/// The runner could not make the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Unreachable;

impl fmt::Display for Unreachable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the model could not be reached")
    }
}

impl Error for Unreachable {}

/// One scripted answer.
#[derive(Debug, Clone)]
pub(crate) enum Answer {
    /// The call ran and came to this.
    Ran(ModelResult),
    /// The runner failed.
    Fails,
    /// The runner panicked.
    Panics,
}

/// When a call may answer.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Hold {
    /// At once.
    Never,
    /// The `bugs` call waits until this many calls have started; the others answer at once.
    FirstUntilStarted(usize),
    /// Every call waits until this many calls have been in flight at once, or `total` calls
    /// have started. Both only grow, so a released call stays released.
    UntilInFlight { in_flight: usize, total: usize },
}

/// Whether a call started or ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    Start(Role),
    End(Role),
}

/// What the runner saw.
#[derive(Debug, Default)]
pub(crate) struct Seen {
    pub(crate) calls: Vec<ModelCall>,
    pub(crate) steps: Vec<Step>,
    pub(crate) in_flight: usize,
    pub(crate) peak: usize,
    pub(crate) started: usize,
}

/// The fake runner.
#[derive(Debug)]
pub(crate) struct Fake {
    answers: Mutex<HashMap<Role, VecDeque<Answer>>>,
    seen: Mutex<Seen>,
    /// Paired with `seen` only.
    changed: Condvar,
    hold: Hold,
}

impl Fake {
    /// A runner answering each role from its script, then with an empty findings document at
    /// a tenth of the role's cap.
    pub(crate) fn new(script: impl IntoIterator<Item = (Role, Vec<Answer>)>, hold: Hold) -> Self {
        Self {
            answers: Mutex::new(
                script
                    .into_iter()
                    .map(|(role, answers)| (role, answers.into()))
                    .collect(),
            ),
            seen: Mutex::new(Seen::default()),
            changed: Condvar::new(),
            hold,
        }
    }

    /// A runner that answers every call at once with the default answer.
    pub(crate) fn answering() -> Self {
        Self::new([], Hold::Never)
    }

    pub(crate) fn seen(&self) -> MutexGuard<'_, Seen> {
        self.seen.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The calls made for `role`, in order.
    pub(crate) fn calls_of(&self, role: Role) -> Vec<ModelCall> {
        self.seen()
            .calls
            .iter()
            .filter(|call| role_of(call) == role)
            .cloned()
            .collect()
    }

    fn released(&self, role: Role, seen: &Seen) -> bool {
        match self.hold {
            Hold::Never => true,
            Hold::FirstUntilStarted(started) => role != Role::Bugs || seen.started >= started,
            Hold::UntilInFlight { in_flight, total } => {
                seen.peak >= in_flight || seen.started >= total
            }
        }
    }
}

impl ModelRunner for Fake {
    type Error = Unreachable;

    fn run(&self, call: &ModelCall) -> Result<ModelResult, Unreachable> {
        let role = role_of(call);
        {
            let mut seen = self.seen();
            // A copy: the test inspects the calls after the fan-out returns.
            seen.calls.push(call.clone());
            seen.steps.push(Step::Start(role));
            seen.in_flight += 1;
            seen.started += 1;
            seen.peak = seen.peak.max(seen.in_flight);
        }
        self.changed.notify_all();
        let (mut seen, _timed_out) = self
            .changed
            .wait_timeout_while(self.seen(), HOLD_LIMIT, |seen| !self.released(role, seen))
            .unwrap_or_else(PoisonError::into_inner);
        seen.in_flight -= 1;
        seen.steps.push(Step::End(role));
        drop(seen);
        self.changed.notify_all();
        let answer = self
            .answers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(&role)
            .and_then(VecDeque::pop_front);
        match answer {
            None => Ok(ran(document_from("combined"), tenth_of(role))),
            Some(Answer::Ran(result)) => Ok(result),
            Some(Answer::Fails) => Err(Unreachable),
            Some(Answer::Panics) => panic!("scripted runner panic"),
        }
    }
}

/// The role a call is for, by the instruction its prompt ends with.
pub(crate) fn role_of(call: &ModelCall) -> Role {
    Role::ALL
        .into_iter()
        .find(|role| call.user_prompt.as_str().ends_with(instruction(*role)))
        .expect("every prompt ends with a role instruction")
}

/// The stand-in instruction of `role` in [`bundle`].
pub(crate) const fn instruction(role: Role) -> &'static str {
    match role {
        Role::Bugs => "BUGS",
        Role::Security => "SECURITY",
        Role::Design => "DESIGN",
        Role::Meta => "META",
        Role::Combined => "COMBINED",
        Role::CombinedMicro => "MICRO",
    }
}

/// The bundle with short stand-in texts.
pub(crate) fn bundle() -> Result<PromptBundle, Box<dyn Error>> {
    Ok(PromptBundle::new(BundleFiles {
        system: "SYSTEM".to_owned(),
        bugs: instruction(Role::Bugs).to_owned(),
        security: instruction(Role::Security).to_owned(),
        design: instruction(Role::Design).to_owned(),
        meta: instruction(Role::Meta).to_owned(),
        combined: instruction(Role::Combined).to_owned(),
        combined_micro: instruction(Role::CombinedMicro).to_owned(),
        verifier: "VERIFIER".to_owned(),
    })?)
}

/// A request for `roles` with the stand-in pack, no intent and the given head start.
pub(crate) fn request<'a>(
    roles: &'a [RequiredRole],
    bundle: &'a PromptBundle,
    head_start: Duration,
) -> SpecialistsRequest<'a> {
    SpecialistsRequest {
        roles,
        context_pack: PACK,
        intent: None,
        bundle,
        json_schema: SCHEMA,
        system_prompt_file: PathBuf::from("/base/plugin/prompts/v1/system.md"),
        add_dir: PathBuf::from("/work/pr"),
        timeout: Duration::from_secs(600),
        head_start,
    }
}

/// A findings document whose `source.role` says `role`, holding `findings`.
pub(crate) fn document_with(role: &str, findings: &[Value]) -> Value {
    json!({
        "schema": "assay.findings/1",
        "source": {
            "kind": "reviewer",
            "name": "claude",
            "version": "git:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "role": role,
            "run_key": "run-42-1",
        },
        "findings": findings,
    })
}

/// An empty findings document whose `source.role` says `role`.
pub(crate) fn document_from(role: &str) -> Value {
    document_with(role, &[])
}

/// A call that produced `output` at `cost`.
pub(crate) fn ran(output: Value, cost: Aeu) -> ModelResult {
    ended(ModelOutcome::Ok(Structured::new(output)), cost)
}

/// A call that ended as `outcome` at `cost`.
pub(crate) fn ended(outcome: ModelOutcome, cost: Aeu) -> ModelResult {
    ModelResult {
        outcome,
        cost,
        rate_limits: Vec::new(),
        init: Err(InitReadError::Missing),
    }
}

/// A tenth of `role`'s cap: what a default answer costs.
pub(crate) const fn tenth_of(role: Role) -> Aeu {
    Aeu::from_micros(role.cap().micros() / 10)
}
