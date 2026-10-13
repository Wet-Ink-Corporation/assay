//! The specialists' fan-out (S4, `docs/architecture/04-pipeline.md` §3, `06-operations.md`
//! §1-§2): the roles a run requires, each one model call through the [`ModelRunner`], run
//! concurrently on scoped threads.
//!
//! At most [`MAX_MODEL_PROCESSES`] calls are in flight. The first call (the first role in
//! roster order) goes alone for a head start, [`HEAD_START`] in production, so the others hit
//! the prompt cache entry it writes: every role shares the system prompt and starts its user
//! prompt with the same context pack. The run cap is the sum of the roles' caps; a role's first
//! call starts only when its cap fits under it beside what is spent and the budgets of the
//! calls in flight, and when the next one can no longer fit with nothing in flight, every role
//! not yet started is skipped. A malformed answer's one retry waits behind every first attempt
//! and gets the smaller of its role's cap and what is left of the run cap, so no call is
//! budgeted past the run cap and a retry never takes a role's room.

mod board;
mod report;
mod schedule;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::thread::{self, Scope, ScopedJoinHandle};
use std::time::Duration;

use assay_domain::RequiredRole;

pub use report::{Halt, RoleFailure, RoleOutcome, RoleReport, Specialists};

use self::board::Board;
use self::schedule::{Joined, Schedule, Start};
use crate::model_runner::{InvalidPrompt, ModelCall, ModelResult, ModelRunner};
use crate::prompt_bundle::{PromptBundle, Role};

/// The most model calls a run has in flight at once (`01` A-03, 06 §1).
pub const MAX_MODEL_PROCESSES: usize = 4;

/// How long the first call runs alone before the others start, so they read its prompt cache
/// entry rather than each writing their own (06 §2). The others start at once if it ends
/// sooner.
pub const HEAD_START: Duration = Duration::from_secs(20);

/// What to run, and how to call the model.
#[derive(Debug)]
pub struct SpecialistsRequest<'a> {
    /// The roles the plan requires (`assay.run/1` `required_roles`); each runs once, in roster
    /// order, whatever the order or repetition here.
    pub roles: &'a [RequiredRole],
    /// The context pack, first in every role's user prompt (04 §3). It must not start with
    /// `-` (see [`Prompt`](crate::Prompt)).
    pub context_pack: &'a str,
    /// The pull request's title, body and commit messages, already inside their untrusted
    /// envelope; only roles that [see the intent](Role::sees_intent) are sent it, after the
    /// pack (04 §3 "Blindness").
    pub intent: Option<&'a str>,
    /// The prompt bundle, whose instruction for each role ends its user prompt.
    pub bundle: &'a PromptBundle,
    /// The JSON Schema of the findings every role returns, as JSON text; one for all roles, so
    /// the prompt cache is shared (03 §7).
    pub json_schema: &'a str,
    /// The base-sourced system prompt file (absolute path), the same for every role.
    pub system_prompt_file: PathBuf,
    /// The read-only PR tree the roles may read (absolute path).
    pub add_dir: PathBuf,
    /// The wall-clock limit of each call, which the runner enforces.
    pub timeout: Duration,
    /// How long the first call runs alone; [`HEAD_START`] in production.
    pub head_start: Duration,
}

/// Runs `request.roles` through `runner`: concurrently, at most [`MAX_MODEL_PROCESSES`] at a
/// time, the first alone for `request.head_start`, under a run cap of the roles' summed caps.
///
/// Each role makes one call with its own turn limit and cap, retried once if its output is
/// malformed (03 §7). Retries queue behind every first attempt, and a retry's budget is the
/// smaller of its role's cap and what is left of the run cap (waiting, while calls are in
/// flight, for them to settle below their reservations); with nothing left, the role is
/// [`Failed`](RoleOutcome::Failed) as malformed without a second call. So no call is given a
/// budget past the run cap, and a retry never costs a role still due its room. A call that
/// reports quota exhausted or a `rejected` or `allowed_warning` rate limit pauses the stage,
/// and a runner that panics stops it: nothing more starts, the roles not yet started are
/// [`Skipped`](RoleOutcome::Skipped), and a retry not yet made leaves its role malformed.
/// Every thread is joined before this returns. A completed role's output has `source.role` set
/// to the role that ran.
///
/// # Errors
/// Returns [`InvalidPrompt`] before any call when a user prompt is not a valid prompt: the
/// context pack starts with `-` or the pack or intent holds a NUL byte. Everything after
/// that is an outcome in the returned [`Specialists`].
///
/// # Examples
///
/// ```
/// use std::convert::Infallible;
/// use std::path::PathBuf;
/// use std::time::Duration;
///
/// use assay_app::{
///     BundleFiles, HEAD_START, InitReadError, ModelCall, ModelOutcome, ModelResult, ModelRunner,
///     PromptBundle, SpecialistsRequest, Structured, run_specialists,
/// };
/// use assay_domain::{Aeu, RequiredRole};
/// use serde_json::json;
///
/// /// A model that finds nothing.
/// struct Quiet;
///
/// impl ModelRunner for Quiet {
///     type Error = Infallible;
///
///     fn run(&self, _call: &ModelCall) -> Result<ModelResult, Infallible> {
///         let document = json!({"source": {"role": "anything"}, "findings": []});
///         Ok(ModelResult {
///             outcome: ModelOutcome::Ok(Structured::new(document)),
///             cost: Aeu::from_micros(100_000),
///             rate_limits: Vec::new(),
///             init: Err(InitReadError::Missing),
///         })
///     }
/// }
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let text = |name: &str| name.to_owned();
/// let bundle = PromptBundle::new(BundleFiles {
///     system: text("system"),
///     bugs: text("bugs"),
///     security: text("security"),
///     design: text("design"),
///     meta: text("meta"),
///     combined: text("combined"),
///     combined_micro: text("micro"),
///     verifier: text("verifier"),
/// })?;
/// let roles = [RequiredRole::Bugs, RequiredRole::Security, RequiredRole::Design];
///
/// let specialists = run_specialists(&Quiet, SpecialistsRequest {
///     roles: &roles,
///     context_pack: "diff --git a/src/lib.rs b/src/lib.rs",
///     intent: None,
///     bundle: &bundle,
///     json_schema: r#"{"type": "object"}"#,
///     system_prompt_file: PathBuf::from("/base/plugin/prompts/v1/system.md"),
///     add_dir: PathBuf::from("/work/pr"),
///     timeout: Duration::from_secs(900),
///     head_start: HEAD_START,
/// })?;
///
/// assert_eq!(specialists.completed_roles(), roles.into());
/// assert_eq!(specialists.cost(), Aeu::from_micros(300_000));
/// # Ok(())
/// # }
/// ```
pub fn run_specialists<R>(
    runner: &R,
    request: SpecialistsRequest<'_>,
) -> Result<Specialists<R::Error>, InvalidPrompt>
where
    R: ModelRunner + Sync,
    R::Error: Send,
{
    let roles: Vec<Role> = request
        .roles
        .iter()
        .copied()
        .collect::<BTreeSet<RequiredRole>>()
        .into_iter()
        .map(Role::from)
        .collect();
    let head_start = request.head_start;
    let calls = calls(&roles, request)?;
    let mut schedule = Schedule::new(&roles);
    let board = Board::default();
    thread::scope(|scope| {
        let mut workers = Workers {
            scope,
            runner,
            calls: calls.into_iter().map(Some).collect(),
            board: &board,
            handles: roles.iter().map(|_| None).collect(),
        };
        // The first call goes alone; the head start ends early if it does.
        workers.start(&mut schedule, 1);
        if schedule.in_flight() > 0 {
            for slot in board.take_within(head_start) {
                workers.settle(&mut schedule, slot);
            }
        }
        loop {
            workers.start(&mut schedule, MAX_MODEL_PROCESSES);
            if schedule.in_flight() == 0 {
                break;
            }
            for slot in board.take() {
                workers.settle(&mut schedule, slot);
            }
        }
    });
    Ok(schedule.finish())
}

/// One call per role, in the roles' order: the pack, the intent for the roles that see it,
/// then the role's instruction, with the role's turns and cap.
fn calls(roles: &[Role], request: SpecialistsRequest<'_>) -> Result<Vec<ModelCall>, InvalidPrompt> {
    let SpecialistsRequest {
        context_pack,
        intent,
        bundle,
        json_schema,
        system_prompt_file,
        add_dir,
        timeout,
        ..
    } = request;
    let with_intent = intent.map(|intent| format!("{context_pack}\n\n{intent}"));
    roles
        .iter()
        .map(|&role| {
            let context = match (&with_intent, role.sees_intent()) {
                (Some(with_intent), true) => with_intent.as_str(),
                _ => context_pack,
            };
            Ok(ModelCall {
                // Each call owns its paths and schema (`ModelCall` stores them); at most six.
                system_prompt_file: system_prompt_file.clone(),
                user_prompt: bundle.user_prompt(role, context)?,
                json_schema: json_schema.to_owned(),
                add_dir: add_dir.clone(),
                max_turns: role.max_turns(),
                budget: role.cap(),
                timeout,
            })
        })
        .collect()
}

/// The thread of one call: the call handed back with the runner's answer, or the runner's
/// panic when joined.
type Handle<'scope, R> =
    ScopedJoinHandle<'scope, (ModelCall, Result<ModelResult, <R as ModelRunner>::Error>)>;

/// The worker threads of one fan-out, each joined by [`Workers::settle`] once it posts.
struct Workers<'scope, 'env, R: ModelRunner> {
    scope: &'scope Scope<'scope, 'env>,
    runner: &'env R,
    /// Each slot's call while no thread holds it: a thread takes it and hands it back when
    /// joined, so a retry reuses it with its own budget.
    calls: Vec<Option<ModelCall>>,
    board: &'env Board,
    /// The thread of each slot's call in flight.
    handles: Vec<Option<Handle<'scope, R>>>,
}

impl<R> Workers<'_, '_, R>
where
    R: ModelRunner + Sync,
    R::Error: Send,
{
    /// Starts every call the schedule allows while fewer than `limit` are in flight.
    fn start(&mut self, schedule: &mut Schedule<R::Error>, limit: usize) {
        while schedule.in_flight() < limit {
            let Some(Start { slot, role, budget }) = schedule.next() else {
                return;
            };
            let calls = self.calls.get_mut(slot).and_then(Option::take);
            let (Some(mut call), Some(handle)) = (calls, self.handles.get_mut(slot)) else {
                // Slots index `calls` and `handles`, both built from the same roles, and a
                // slot's call is back before the schedule starts it again, so this does not
                // happen. Were it to, the role reports `Spawn(NotFound)`: its call never
                // started, nothing ran and nothing was charged, as if the OS had refused the
                // thread, but the fault is this bookkeeping, not the OS.
                schedule.refused(slot, std::io::ErrorKind::NotFound);
                continue;
            };
            call.budget = budget;
            let (runner, board) = (self.runner, self.board);
            let spawned = thread::Builder::new()
                .name(format!("assay-{}", role.code()))
                .spawn_scoped(self.scope, move || {
                    // Posts when the call returns or the runner unwinds, so the coordinator
                    // never waits on a thread that is gone.
                    let _posted = board.poster(slot);
                    let returned = runner.run(&call);
                    (call, returned)
                });
            match spawned {
                Ok(spawned) => *handle = Some(spawned),
                Err(error) => schedule.refused(slot, error.kind()),
            }
        }
    }

    /// Joins `slot`'s thread, which has posted, and settles its call.
    fn settle(&mut self, schedule: &mut Schedule<R::Error>, slot: usize) {
        let Some(handle) = self.handles.get_mut(slot).and_then(Option::take) else {
            return;
        };
        let joined = match handle.join() {
            Ok((call, returned)) => {
                if let Some(place) = self.calls.get_mut(slot) {
                    *place = Some(call);
                }
                Joined::Returned(returned)
            }
            // The payload is the runner's panic message; the role reports the panic itself.
            Err(_payload) => Joined::Panicked,
        };
        schedule.settle(slot, joined);
    }
}
