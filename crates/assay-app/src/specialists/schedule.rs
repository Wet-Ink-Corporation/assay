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
}

/// Where a role is.
enum State<E> {
    /// Queued for its first attempt, or never started.
    Due,
    /// A call is in flight.
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
    report: RoleReport<E>,
}

/// The run cap's accounting: what settled calls cost, and the caps of the calls in flight.
#[derive(Debug, Clone, Copy)]
struct Ledger {
    cap: Aeu,
    spent: Aeu,
    reserved: Aeu,
}

impl Ledger {
    /// Whether a call capped at `call` may start: everything settled, every cap in flight and
    /// this cap together stay within the run cap.
    fn fits(self, call: Aeu) -> bool {
        self.spent
            .saturating_add(self.reserved)
            .saturating_add(call)
            <= self.cap
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
    /// Slots whose malformed first answer is to be retried, in the order the answers came back.
    retries: VecDeque<usize>,
    in_flight: usize,
    ledger: Ledger,
    halt: Option<Halt>,
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
            ledger: Ledger {
                cap,
                spent: Aeu::ZERO,
                reserved: Aeu::ZERO,
            },
            halt: None,
        }
    }

    /// The calls in flight.
    pub(super) const fn in_flight(&self) -> usize {
        self.in_flight
    }

    /// The next call to start, if one may, with its cap reserved: none while
    /// [`MAX_MODEL_PROCESSES`] calls are in flight or once the stage paused or stopped.
    ///
    /// A queued retry goes first and is not held to the run cap: 06 §1 skips remaining roles,
    /// and a role already started keeps its one retry (03 §7). A first attempt starts only when
    /// its cap fits under the run cap beside what is spent and reserved; while it does not,
    /// none does, and when nothing is in flight to settle below its reservation, the run cap
    /// halts every first attempt left.
    pub(super) fn next(&mut self) -> Option<(usize, Role)> {
        let stopped = matches!(self.halt, Some(Halt::RateLimited | Halt::Panicked));
        if stopped || self.in_flight >= MAX_MODEL_PROCESSES {
            return None;
        }
        let slot = match self.retries.pop_front() {
            Some(slot) => slot,
            None => self.next_due()?,
        };
        let entry = self.entries.get_mut(slot)?;
        let cap = entry.role.cap();
        self.ledger.reserve(cap);
        self.in_flight = self.in_flight.saturating_add(1);
        entry.state = State::Running;
        entry.report.attempts = entry.report.attempts.saturating_add(1);
        Some((slot, entry.role))
    }

    /// Takes the next first attempt if its cap fits under the run cap; halts on the run cap
    /// when it does not and nothing in flight could settle below its reservation.
    fn next_due(&mut self) -> Option<usize> {
        if self.halt.is_some() {
            return None;
        }
        let slot = *self.due.front()?;
        let cap = self.entries.get(slot)?.role.cap();
        if !self.ledger.fits(cap) {
            if self.in_flight == 0 {
                self.halt = Some(Halt::RunCap);
            }
            return None;
        }
        self.due.pop_front()
    }

    /// The thread for `slot`'s call could not be started: nothing ran, so nothing is charged
    /// and the role fails.
    pub(super) fn refused(&mut self, slot: usize, kind: std::io::ErrorKind) {
        self.in_flight = self.in_flight.saturating_sub(1);
        if let Some(entry) = self.entries.get_mut(slot) {
            self.ledger.settle(entry.role.cap(), Aeu::ZERO);
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
        let cap = entry.role.cap();
        let report = &mut entry.report;
        let result = match joined {
            Joined::Panicked => {
                self.ledger.settle(cap, cap);
                report.cost = report.cost.saturating_add(cap);
                entry.state = State::Done(RoleOutcome::Failed(RoleFailure::Panicked));
                self.halt.get_or_insert(Halt::Panicked);
                return;
            }
            Joined::Returned(Err(source)) => {
                // The model may have started; its cost is unknown, so the cap is charged.
                self.ledger.settle(cap, cap);
                report.cost = report.cost.saturating_add(cap);
                entry.state = State::Done(RoleOutcome::Failed(RoleFailure::Runner(source)));
                return;
            }
            Joined::Returned(Ok(result)) => result,
        };
        self.ledger.settle(cap, result.cost);
        report.cost = report.cost.saturating_add(result.cost);
        if !result.rate_limits.is_empty() || result.outcome == ModelOutcome::QuotaExhausted {
            self.halt.get_or_insert(Halt::RateLimited);
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
        let skipped = self.halt.unwrap_or(Halt::RunCap);
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
        }
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
