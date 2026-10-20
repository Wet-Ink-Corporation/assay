//! `admit`'s `assay.event/1` stream (03 §6): job `admit`, stage `S1a`, numbered from 0 in the
//! order its events happen. Events carry only assay's own words: codes, run ids and amounts.

use assay_domain::{Event, EventPayload, EventSchema, RunUid};

use crate::events::EventSink;
use crate::status::FetchedAt;

/// The job whose stream this is.
const JOB: &str = "admit";

/// Its pipeline stage (04 §1).
const STAGE: &str = "S1a";

/// Why an event was not recorded; `admit` stops on either.
#[derive(Debug)]
pub(super) enum EventFailure<E> {
    /// The sink refused it.
    Sink(E),
    /// The sink's clock read before 1970 or after 9999, which no `ts` can say.
    Clock,
}

/// Builds each envelope and appends it to the sink.
pub(super) struct Recorder<'e, S> {
    sink: &'e mut S,
    run_key: String,
    run_uid: RunUid,
    next: u64,
}

impl<'e, S: EventSink> Recorder<'e, S> {
    /// A recorder of the run `run_key` and `run_uid` into `sink`.
    pub(super) const fn new(sink: &'e mut S, run_key: String, run_uid: RunUid) -> Self {
        Self {
            sink,
            run_key,
            run_uid,
            next: 0,
        }
    }

    /// Appends `payload` as the stream's next event, stamped now.
    pub(super) fn emit(&mut self, payload: EventPayload) -> Result<(), EventFailure<S::Error>> {
        let ts =
            FetchedAt::from_system_time(self.sink.now()).map_err(|_range| EventFailure::Clock)?;
        let event = Event {
            schema: EventSchema::V1,
            job: JOB.to_owned(),
            idx: self.next,
            ts: ts.into(),
            // Each event owns its copy of the run key.
            run_key: self.run_key.clone(),
            run_uid: self.run_uid,
            stage: STAGE.to_owned(),
            payload,
        };
        self.next = self.next.saturating_add(1);
        self.sink.append(&event).map_err(EventFailure::Sink)
    }
}
