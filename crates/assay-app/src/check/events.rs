//! The local run's `assay.event/1` stream (#130, 03 §6): one stream per job, each numbered from
//! 0 in the order its events happen, with the job's pipeline stage (04 §1).
//!
//! Events carry only assay's own words (role and plugin names, codes, numbers), never model or
//! gate text.

use std::time::{Duration, SystemTime};

use assay_domain::{Aeu, Event, EventPayload, EventSchema, GateRun, RunUid};

use crate::config::Plugin;
use crate::events::EventSink;
use crate::gate::{BuildJob, GateJob, GateJobOutcome, GateJobReport, GateObserver};
use crate::status::FetchedAt;

/// Micro-AEU in one AEU.
const MICROS_PER_AEU: u64 = 1_000_000;

/// A job of the local run, which names its stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Job {
    /// `plan` (S1).
    Plan,
    /// One of the gate jobs (S2s, S2b).
    Gates(GateJob),
    /// `review` (S4): the one combined call.
    Review,
    /// `decide` (S7).
    Decide,
}

impl Job {
    const fn name(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Gates(GateJob::Static) => "gates-static",
            Self::Gates(GateJob::Build(BuildJob::Lint)) => "gates-lint",
            Self::Gates(GateJob::Build(BuildJob::Test)) => "gates-test",
            Self::Review => "review",
            Self::Decide => "decide",
        }
    }

    const fn stage(self) -> &'static str {
        match self {
            Self::Plan => "S1",
            Self::Gates(GateJob::Static) => "S2s",
            Self::Gates(GateJob::Build(_)) => "S2b",
            Self::Review => "S4",
            Self::Decide => "S7",
        }
    }
}

/// Why an event was not recorded. The check stops on either.
#[derive(Debug)]
pub(super) enum EventFailure<E> {
    /// The sink refused it.
    Sink(E),
    /// The clock read before 1970 or after 9999, which no `ts` can say.
    Clock,
}

/// The next `idx` of each job's stream.
#[derive(Debug, Default)]
struct Next {
    plan: u64,
    gates_static: u64,
    gates_lint: u64,
    gates_test: u64,
    review: u64,
    decide: u64,
}

impl Next {
    /// `job`'s next index, which is then used up.
    const fn take(&mut self, job: Job) -> u64 {
        let next = match job {
            Job::Plan => &mut self.plan,
            Job::Gates(GateJob::Static) => &mut self.gates_static,
            Job::Gates(GateJob::Build(BuildJob::Lint)) => &mut self.gates_lint,
            Job::Gates(GateJob::Build(BuildJob::Test)) => &mut self.gates_test,
            Job::Review => &mut self.review,
            Job::Decide => &mut self.decide,
        };
        let idx = *next;
        *next = idx.saturating_add(1);
        idx
    }
}

/// Builds each envelope and appends it to the sink.
pub(super) struct Recorder<'e, S> {
    sink: &'e mut S,
    run_key: String,
    run_uid: RunUid,
    next: Next,
}

impl<'e, S: EventSink> Recorder<'e, S> {
    /// A recorder of the run `run_key` and `run_uid` into `sink`.
    pub(super) fn new(sink: &'e mut S, run_key: String, run_uid: RunUid) -> Self {
        Self {
            sink,
            run_key,
            run_uid,
            next: Next::default(),
        }
    }

    /// The sink's clock.
    pub(super) fn now(&self) -> SystemTime {
        self.sink.now()
    }

    /// Appends `payload` as `job`'s next event, stamped now.
    pub(super) fn emit(
        &mut self,
        job: Job,
        payload: EventPayload,
    ) -> Result<(), EventFailure<S::Error>> {
        let now = self.sink.now();
        self.emit_at(job, payload, now)
    }

    /// Appends `payload` as `job`'s next event, stamped `at`: a start or end is stamped with
    /// the reading its duration is measured from.
    pub(super) fn emit_at(
        &mut self,
        job: Job,
        payload: EventPayload,
        at: SystemTime,
    ) -> Result<(), EventFailure<S::Error>> {
        let ts = FetchedAt::from_system_time(at).map_err(|_out_of_range| EventFailure::Clock)?;
        let event = Event {
            schema: EventSchema::V1,
            job: job.name().to_owned(),
            idx: self.next.take(job),
            ts: ts.into(),
            // Each event owns its copy of the run key.
            run_key: self.run_key.clone(),
            run_uid: self.run_uid,
            stage: job.stage().to_owned(),
            payload,
        };
        self.sink.append(&event).map_err(EventFailure::Sink)
    }

    /// Each payload of `payloads` as `job`'s next events, in order, all stamped `at`.
    pub(super) fn emit_all_at(
        &mut self,
        job: Job,
        payloads: impl IntoIterator<Item = EventPayload>,
        at: SystemTime,
    ) -> Result<(), EventFailure<S::Error>> {
        payloads
            .into_iter()
            .try_for_each(|payload| self.emit_at(job, payload, at))
    }
}

/// Records one gate job's `GateStarted` and `GateFinished` events.
pub(super) struct GateEvents<'r, 'e, S> {
    recorder: &'r mut Recorder<'e, S>,
    job: GateJob,
    /// When the gate now running started.
    started: Option<SystemTime>,
}

impl<'r, 'e, S: EventSink> GateEvents<'r, 'e, S> {
    pub(super) const fn new(recorder: &'r mut Recorder<'e, S>, job: GateJob) -> Self {
        Self {
            recorder,
            job,
            started: None,
        }
    }
}

impl<S: EventSink> GateObserver for GateEvents<'_, '_, S> {
    type Error = EventFailure<S::Error>;

    fn job(&self) -> GateJob {
        self.job
    }

    fn started(&mut self, plugin: &Plugin) -> Result<(), Self::Error> {
        let now = self.recorder.now();
        self.started = Some(now);
        let run = GateRun {
            plugin: plugin.name().as_str().to_owned(),
            exit: None,
            conclusion: None,
            duration_ms: None,
        };
        self.recorder
            .emit_at(Job::Gates(self.job), EventPayload::GateStarted(run), now)
    }

    /// A gate that never started (a required one with no report) has no duration.
    fn ended(&mut self, report: &GateJobReport, exit: Option<i32>) -> Result<(), Self::Error> {
        let now = self.recorder.now();
        let duration_ms = self
            .started
            .take()
            .map(|started| millis_since(started, now));
        let conclusion = match report.outcome {
            GateJobOutcome::Concluded(conclusion) => Some(conclusion),
            GateJobOutcome::Error(_) => None,
        };
        let run = GateRun {
            plugin: report.plugin.as_str().to_owned(),
            exit: exit.map(i64::from),
            conclusion,
            duration_ms,
        };
        self.recorder
            .emit_at(Job::Gates(self.job), EventPayload::GateFinished(run), now)
    }
}

/// Whole milliseconds from `started` to `now`; zero when the clock stepped back.
pub(super) fn millis_since(started: SystemTime, now: SystemTime) -> u64 {
    let elapsed = now.duration_since(started).unwrap_or(Duration::ZERO);
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

/// `cost` as the number events carry; `None` past the AEU an `f64` holds exactly as whole units.
pub(super) fn aeu_number(cost: Aeu) -> Option<f64> {
    let micros = cost.micros();
    let whole = u32::try_from(micros / MICROS_PER_AEU).ok()?;
    let fraction = u32::try_from(micros % MICROS_PER_AEU).ok()?;
    Some(f64::from(whole) + f64::from(fraction) / 1_000_000.0)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use assay_domain::Aeu;

    use super::{aeu_number, millis_since};

    #[test]
    fn a_cost_is_its_aeu_as_a_number() {
        let costs = [0, 50_000, 1_250_000].map(|micros| aeu_number(Aeu::from_micros(micros)));

        assert_eq!(costs, [Some(0.0), Some(0.05), Some(1.25)]);
    }

    #[test]
    fn a_cost_past_u32_whole_aeu_has_no_number() {
        let largest = u64::from(u32::MAX) * 1_000_000 + 999_999;

        let numbers = [largest, largest + 1].map(|micros| aeu_number(Aeu::from_micros(micros)));

        assert_eq!(numbers.map(|number| number.is_some()), [true, false]);
    }

    #[test]
    fn a_duration_is_whole_milliseconds_and_zero_when_the_clock_stepped_back() {
        let start = SystemTime::UNIX_EPOCH + Duration::from_secs(10);
        let later = start + Duration::from_micros(2_500);

        assert_eq!(
            (millis_since(start, later), millis_since(later, start)),
            (2, 0)
        );
    }
}
