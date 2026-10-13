//! The fan-out's decisions, free of threads: which call starts next, what the run cap allows,
//! and what each ended call means for its role (04 §3, 06 §1, 03 §7).

use std::collections::VecDeque;

use assay_domain::{Aeu, CallFailure};
use serde_json::Value;

use super::MAX_MODEL_PROCESSES;
use super::report::{Halt, RoleFailure, RoleOutcome, RoleReport, Specialists};
use crate::model_runner::{ModelOutcome, ModelResult, Structured};
use crate::prompt_bundle::Role;

/// Attempts per role: the first, and one retry of malformed output (03 §7).
const ATTEMPTS: u8 = 2;

/// How a worker's call came back.
pub(super) enum Joined<E> {
    /// The runner returned.
    Returned(Result<ModelResult, E>),
    /// The runner panicked.
    Panicked,
    /// The fan-out lost the call's thread: a slot posted with no handle to join. Its cost is
    /// unknown, so it is charged its budget.
    Lost,
}

/// A call the schedule starts: the slot and role it is for, and its cap (`--max-budget-usd`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Start {
    pub(super) slot: usize,
    pub(super) role: Role,
    /// The role's cap for a first attempt; for a retry, at most what is left of the run cap.
    pub(super) budget: Aeu,
}

/// Where a role is.
enum State<E> {
    /// Queued for its first attempt, or never started.
    Due,
    /// A call is in flight, reserving its budget under the run cap.
    Running,
    /// Its first answer was malformed; the retry is queued.
    Retrying,
    /// Settled.
    Done(RoleOutcome<E>),
}

/// One role's progress and what it has spent.
struct Entry<E> {
    role: Role,
    state: State<E>,
    /// The budget of its latest call: what that call reserves while in flight, and what it is
    /// charged when its cost is unknown.
    budget: Aeu,
    report: RoleReport<E>,
}

/// What the queue of first attempts allows next.
enum FirstAttempt {
    /// This slot's first attempt fits under the run cap.
    Start(usize),
    /// The next one does not fit yet, and calls in flight may settle below their reservations.
    Wait,
    /// None is due: all started, or the run cap halted the rest.
    NoneLeft,
}

/// The run cap's accounting: what settled calls cost, and the budgets of the calls in flight.
#[derive(Debug, Clone, Copy)]
struct Ledger {
    cap: Aeu,
    spent: Aeu,
    reserved: Aeu,
}

impl Ledger {
    /// What is left of the run cap beside what settled calls cost and what calls in flight
    /// reserve.
    fn left(self) -> Aeu {
        let used = self.spent.saturating_add(self.reserved);
        Aeu::from_micros(self.cap.micros().saturating_sub(used.micros()))
    }

    /// Whether a call capped at `call` may start: everything settled, every budget in flight
    /// and this cap together stay within the run cap.
    fn fits(self, call: Aeu) -> bool {
        call <= self.left()
    }

    fn reserve(&mut self, call: Aeu) {
        self.reserved = self.reserved.saturating_add(call);
    }

    /// Releases the reservation of a call capped at `call` and charges `cost` instead.
    fn settle(&mut self, call: Aeu, cost: Aeu) {
        self.reserved = Aeu::from_micros(self.reserved.micros().saturating_sub(call.micros()));
        self.spent = self.spent.saturating_add(cost);
    }
}

/// The schedule of one fan-out: the roles' queues, the calls in flight and the run cap.
pub(super) struct Schedule<E> {
    entries: Vec<Entry<E>>,
    /// Slots whose first attempt is due, in roster order.
    due: VecDeque<usize>,
    /// Slots whose malformed first answer is to be retried, in the order the answers came back;
    /// they start only once no first attempt is due.
    retries: VecDeque<usize>,
    in_flight: usize,
    /// The first attempts among the calls in flight: only they make a retry wait (#217).
    first_in_flight: usize,
    ledger: Ledger,
    /// The strongest reason the stage stopped starting calls (see [`raise`]).
    halt: Option<Halt>,
    /// The first reason: what the roles still due were skipped for.
    skipped: Option<Halt>,
}

impl<E> Schedule<E> {
    /// A schedule for `roles`, distinct and in roster order, under the sum of their caps.
    pub(super) fn new(roles: &[Role]) -> Self {
        let cap = roles
            .iter()
            .fold(Aeu::ZERO, |sum, role| sum.saturating_add(role.cap()));
        Self {
            entries: roles
                .iter()
                .map(|&role| Entry {
                    role,
                    state: State::Due,
                    budget: role.cap(),
                    report: RoleReport {
                        role,
                        outcome: RoleOutcome::Skipped(Halt::RunCap),
                        attempts: 0,
                        cost: Aeu::ZERO,
                        rate_limits: Vec::new(),
                        init: None,
                    },
                })
                .collect(),
            due: (0..roles.len()).collect(),
            retries: VecDeque::new(),
            in_flight: 0,
            first_in_flight: 0,
            ledger: Ledger {
                cap,
                spent: Aeu::ZERO,
                reserved: Aeu::ZERO,
            },
            halt: None,
            skipped: None,
        }
    }

    /// A schedule for `roles` that starts nothing: the stage halted for `halt` before it.
    pub(super) fn halted(roles: &[Role], halt: Halt) -> Self {
        let mut schedule = Self::new(roles);
        schedule.halt = Some(halt);
        schedule.skipped = Some(halt);
        schedule
    }

    /// The calls in flight.
    pub(super) const fn in_flight(&self) -> usize {
        self.in_flight
    }

    /// The next call to start, if one may, with its budget reserved: none while
    /// [`MAX_MODEL_PROCESSES`] calls are in flight or once the stage paused or stopped.
    ///
    /// First attempts go first, in roster order, each with its role's cap, and only when that
    /// cap fits under the run cap beside what is spent and reserved; while it does not, none
    /// starts, and when nothing is in flight to settle below its reservation, the run cap halts
    /// every first attempt left (06 §1). Retries queue behind every first attempt, so a retry
    /// never takes the room of a role still due (03 §7).
    pub(super) fn next(&mut self) -> Option<Start> {
        let stopped = matches!(self.halt, Some(Halt::RateLimited | Halt::Panicked));
        if stopped || self.in_flight >= MAX_MODEL_PROCESSES {
            return None;
        }
        let (slot, budget) = match self.next_due() {
            FirstAttempt::Start(slot) => (slot, self.entries.get(slot)?.role.cap()),
            FirstAttempt::Wait => return None,
            FirstAttempt::NoneLeft => self.next_retry()?,
        };
        let entry = self.entries.get_mut(slot)?;
        self.ledger.reserve(budget);
        self.in_flight = self.in_flight.saturating_add(1);
        if entry.report.attempts == 0 {
            self.first_in_flight = self.first_in_flight.saturating_add(1);
        }
        entry.state = State::Running;
        entry.budget = budget;
        entry.report.attempts = entry.report.attempts.saturating_add(1);
        Some(Start {
            slot,
            role: entry.role,
            budget,
        })
    }

    /// Takes the next first attempt if its cap fits under the run cap; halts on the run cap
    /// when it does not and nothing in flight could settle below its reservation.
    fn next_due(&mut self) -> FirstAttempt {
        if self.halt.is_some() {
            return FirstAttempt::NoneLeft;
        }
        let Some((slot, cap)) = self
            .due
            .front()
            .and_then(|&slot| Some((slot, self.entries.get(slot)?.role.cap())))
        else {
            return FirstAttempt::NoneLeft;
        };
        if self.ledger.fits(cap) {
            self.due.pop_front();
            FirstAttempt::Start(slot)
        } else if self.in_flight == 0 {
            self.halt = Some(Halt::RunCap);
            self.skipped = Some(Halt::RunCap);
            FirstAttempt::NoneLeft
        } else {
            FirstAttempt::Wait
        }
    }

    /// Takes the next retry with its budget, the smaller of its role's cap and what is left of
    /// the run cap (as the verifier's retries, 04 §4 step 8). While the cap does not fit and
    /// first attempts in flight may settle below their reservations, it waits; otherwise it
    /// starts with what is left, and with nothing left the role fails as malformed and the next
    /// retry is looked at. It never waits on another retry, so retries cannot serialise behind
    /// each other's timeouts (#217): a retry behind a retry takes a smaller budget instead.
    fn next_retry(&mut self) -> Option<(usize, Aeu)> {
        while let Some(&slot) = self.retries.front() {
            let entry = self.entries.get_mut(slot)?;
            let cap = entry.role.cap();
            let left = self.ledger.left();
            if cap > left && self.first_in_flight > 0 {
                return None;
            }
            self.retries.pop_front();
            if left > Aeu::ZERO {
                return Some((slot, cap.min(left)));
            }
            entry.state = State::Done(RoleOutcome::Failed(RoleFailure::Call(
                CallFailure::Malformed,
            )));
        }
        None
    }

    /// The thread for `slot`'s call could not be started: nothing ran, so nothing is charged
    /// and the role fails.
    pub(super) fn refused(&mut self, slot: usize, kind: std::io::ErrorKind) {
        self.in_flight = self.in_flight.saturating_sub(1);
        if let Some(entry) = self.entries.get_mut(slot) {
            self.first_in_flight = ended_first(self.first_in_flight, entry.report.attempts);
            self.ledger.settle(entry.budget, Aeu::ZERO);
            entry.report.attempts = entry.report.attempts.saturating_sub(1);
            entry.state = State::Done(RoleOutcome::Failed(RoleFailure::Spawn(kind)));
        }
    }

    /// Settles `slot`'s call: charges it, keeps its rate limits and init, and decides the role
    /// (03 §7: only malformed output is retried, once). Quota and rate-limit signals pause the
    /// stage, and a panic stops it.
    pub(super) fn settle(&mut self, slot: usize, joined: Joined<E>) {
        self.in_flight = self.in_flight.saturating_sub(1);
        let Some(entry) = self.entries.get_mut(slot) else {
            return;
        };
        self.first_in_flight = ended_first(self.first_in_flight, entry.report.attempts);
        let budget = entry.budget;
        let report = &mut entry.report;
        let result = match joined {
            Joined::Panicked => {
                self.ledger.settle(budget, budget);
                report.cost = report.cost.saturating_add(budget);
                entry.state = State::Done(RoleOutcome::Failed(RoleFailure::Panicked));
                self.halt = Some(raise(self.halt, Halt::Panicked));
                self.skipped.get_or_insert(Halt::Panicked);
                return;
            }
            Joined::Lost => {
                self.ledger.settle(budget, budget);
                report.cost = report.cost.saturating_add(budget);
                entry.state = State::Done(RoleOutcome::Failed(RoleFailure::Lost));
                return;
            }
            Joined::Returned(Err(source)) => {
                // The model may have started; its cost is unknown, so its budget is charged.
                self.ledger.settle(budget, budget);
                report.cost = report.cost.saturating_add(budget);
                entry.state = State::Done(RoleOutcome::Failed(RoleFailure::Runner(source)));
                return;
            }
            Joined::Returned(Ok(result)) => result,
        };
        self.ledger.settle(budget, result.cost);
        report.cost = report.cost.saturating_add(result.cost);
        if !result.rate_limits.is_empty() || result.outcome == ModelOutcome::QuotaExhausted {
            self.halt = Some(raise(self.halt, Halt::RateLimited));
            self.skipped.get_or_insert(Halt::RateLimited);
        }
        report.rate_limits.extend(result.rate_limits);
        report.init = Some(result.init);
        let failure = match result.outcome {
            ModelOutcome::Ok(structured) => {
                entry.state = State::Done(RoleOutcome::Completed(stamp(structured, entry.role)));
                return;
            }
            ModelOutcome::Malformed if report.attempts < ATTEMPTS => {
                entry.state = State::Retrying;
                self.retries.push_back(slot);
                return;
            }
            ModelOutcome::Malformed => CallFailure::Malformed,
            ModelOutcome::BudgetExhausted => CallFailure::Cap,
            ModelOutcome::TurnsExhausted => CallFailure::Turns,
            ModelOutcome::Timeout => CallFailure::Timeout,
            ModelOutcome::QuotaExhausted => CallFailure::Unavailable,
        };
        entry.state = State::Done(RoleOutcome::Failed(RoleFailure::Call(failure)));
    }

    /// The reports, once nothing is in flight. A role still due was skipped for the reason
    /// the stage halted; `next` sets one whenever it stops with first attempts due and none in
    /// flight, so the run cap stands in only for a caller that stopped early. A retry the
    /// paused or stopped stage never made leaves the role with its malformed answer.
    pub(super) fn finish(self) -> Specialists<E> {
        let skipped = self.skipped.unwrap_or(Halt::RunCap);
        let roles = self
            .entries
            .into_iter()
            .map(|entry| {
                let outcome = match entry.state {
                    State::Done(outcome) => outcome,
                    State::Retrying => {
                        RoleOutcome::Failed(RoleFailure::Call(CallFailure::Malformed))
                    }
                    State::Due | State::Running => RoleOutcome::Skipped(skipped),
                };
                RoleReport {
                    outcome,
                    ..entry.report
                }
            })
            .collect();
        Specialists {
            roles,
            run_cap: self.ledger.cap,
            halt: self.halt,
        }
    }
}

/// The stronger of the stage's halt so far and `halt`: a run-cap halt is a reason to start no
/// first attempt, but a pause or panic after it (in a retry) must still stop later shards, so
/// `RunCap < RateLimited < Panicked` (#98 review).
const fn raise(current: Option<Halt>, halt: Halt) -> Halt {
    const fn rank(halt: Halt) -> u8 {
        match halt {
            Halt::RunCap => 0,
            Halt::RateLimited => 1,
            Halt::Panicked => 2,
        }
    }
    match current {
        Some(current) if rank(current) >= rank(halt) => current,
        _ => halt,
    }
}

/// The first attempts in flight once a call whose role has made `attempts` attempts ends: one
/// fewer when it was the role's first.
const fn ended_first(first_in_flight: usize, attempts: u8) -> usize {
    if attempts == 1 {
        first_in_flight.saturating_sub(1)
    } else {
        first_in_flight
    }
}

/// `structured` with `source.role` set to the role that ran, whatever the model wrote there
/// (#173): validation applies the role's severity ceiling by that field. Output without a
/// `source` object is left as it is, for validation to refuse.
fn stamp(structured: Structured, role: Role) -> Structured {
    let mut output = structured.into_value();
    if let Some(source) = output.get_mut("source").and_then(Value::as_object_mut) {
        source.insert("role".to_owned(), Value::from(role.code()));
    }
    Structured::new(output)
}

#[cfg(test)]
mod tests;
