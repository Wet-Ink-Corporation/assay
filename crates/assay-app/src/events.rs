//! The `EventSink` port: where a run appends its `assay.event/1` stream
//! (`docs/architecture/03-contracts.md` §6).
//!
//! The use case builds each envelope (job, `idx`, stage, run key) and the sink only stores or
//! writes it. The sink also reads the wall clock, for each event's `ts` and the durations events
//! report, so a test can run on a fake clock (TST-10).

use std::convert::Infallible;
use std::time::SystemTime;

use assay_domain::Event;

/// A run's event stream.
pub trait EventSink {
    /// Why an event could not be appended.
    type Error: std::error::Error;

    /// The wall-clock time now.
    fn now(&self) -> SystemTime;

    /// Appends `event` to the stream.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the event could not be written; the run stops.
    fn append(&mut self, event: &Event) -> Result<(), Self::Error>;
}

/// No stream: every event is dropped, as no one asked for them. Its clock reads the Unix epoch,
/// so nothing it is given depends on the time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NoEvents;

impl EventSink for NoEvents {
    type Error = Infallible;

    fn now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
    }

    fn append(&mut self, _event: &Event) -> Result<(), Self::Error> {
        Ok(())
    }
}
