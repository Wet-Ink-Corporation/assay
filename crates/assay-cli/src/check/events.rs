//! The event stream of `assay check --local --events <path|->` (#130): each `assay.event/1`
//! envelope as one line of compact JSON, flushed as it is written, so a reader (the mod's live
//! view) sees each stage as it happens.
//!
//! Every write is bounded: a line over [`MAX_EVENT_LINE_BYTES`] or a stream past
//! [`MAX_STREAM_BYTES`] stops the check rather than being cut. Events hold only assay's own
//! words, so neither cap is reached by a real run.

use std::fmt;
use std::io::{self, Write};
use std::time::SystemTime;

use assay_app::EventSink;
use assay_domain::Event;

/// The longest event line written, in bytes, its newline excluded.
pub(crate) const MAX_EVENT_LINE_BYTES: usize = 64 * 1024;

/// The most bytes one stream holds, newlines included.
pub(crate) const MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;

/// Why an event was not written.
#[derive(Debug)]
pub(crate) enum EventWriteError {
    /// The event could not be encoded.
    Encode(serde_json::Error),
    /// Its line is over [`MAX_EVENT_LINE_BYTES`].
    LineTooLong { bytes: usize },
    /// It would take the stream past [`MAX_STREAM_BYTES`].
    StreamFull,
    /// The stream could not be written or flushed (a closed pipe, a full disk).
    Write(io::Error),
}

impl fmt::Display for EventWriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Encode(source) => write!(f, "cannot encode an event: {source}"),
            Self::LineTooLong { bytes } => write!(
                f,
                "an event line of {bytes} bytes is over the {MAX_EVENT_LINE_BYTES}-byte cap"
            ),
            Self::StreamFull => write!(
                f,
                "the event stream would pass its {MAX_STREAM_BYTES}-byte cap"
            ),
            Self::Write(source) => write!(f, "cannot write the event stream: {source}"),
        }
    }
}

impl std::error::Error for EventWriteError {}

/// Where the events go: nowhere, or one JSONL stream.
pub(crate) struct JsonlEvents<W> {
    out: Option<W>,
    written: usize,
    line_cap: usize,
    stream_cap: usize,
}

impl<W: Write> JsonlEvents<W> {
    /// No stream: `--events` was not given.
    pub(crate) const fn none() -> Self {
        Self::with_caps(None, MAX_EVENT_LINE_BYTES, MAX_STREAM_BYTES)
    }

    /// A stream into `out`.
    pub(crate) const fn to(out: W) -> Self {
        Self::with_caps(Some(out), MAX_EVENT_LINE_BYTES, MAX_STREAM_BYTES)
    }

    const fn with_caps(out: Option<W>, line_cap: usize, stream_cap: usize) -> Self {
        Self {
            out,
            written: 0,
            line_cap,
            stream_cap,
        }
    }
}

impl<W: Write> EventSink for JsonlEvents<W> {
    type Error = EventWriteError;

    fn now(&self) -> SystemTime {
        SystemTime::now()
    }

    fn append(&mut self, event: &Event) -> Result<(), EventWriteError> {
        let Some(out) = self.out.as_mut() else {
            return Ok(());
        };
        let mut line = serde_json::to_vec(event).map_err(EventWriteError::Encode)?;
        if line.len() > self.line_cap {
            return Err(EventWriteError::LineTooLong { bytes: line.len() });
        }
        line.push(b'\n');
        let written = self
            .written
            .checked_add(line.len())
            .filter(|total| *total <= self.stream_cap)
            .ok_or(EventWriteError::StreamFull)?;
        out.write_all(&line)
            .and_then(|()| out.flush())
            .map_err(EventWriteError::Write)?;
        self.written = written;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};
    use std::num::NonZeroU64;

    use assay_app::EventSink;
    use assay_domain::{Event, EventPayload, EventSchema, RunUid};

    use super::{EventWriteError, JsonlEvents};

    /// A `StageDegraded` event whose compact line is `reason_len` bytes longer than with an
    /// empty reason.
    fn event(reason_len: usize) -> Event {
        Event {
            schema: EventSchema::V1,
            job: "review".to_owned(),
            idx: 0,
            ts: "2026-10-13T09:30:00.000Z".to_owned(),
            run_key: "local".to_owned(),
            run_uid: RunUid {
                repo_id: NonZeroU64::MIN,
                run_id: NonZeroU64::MIN,
                run_attempt: NonZeroU64::MIN,
            },
            stage: "S4".to_owned(),
            payload: EventPayload::StageDegraded {
                stage: "review".to_owned(),
                reason: "r".repeat(reason_len),
            },
        }
    }

    /// The compact line of `event`, newline excluded.
    fn line_len(event: &Event) -> usize {
        serde_json::to_vec(event).map_or(0, |line| line.len())
    }

    /// A writer that keeps what it is given and counts flushes.
    #[derive(Default)]
    struct Kept {
        bytes: Vec<u8>,
        flushes: usize,
    }

    impl Write for &mut Kept {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    #[test]
    fn each_event_is_one_compact_line_flushed_as_it_is_written() {
        let mut kept = Kept::default();
        let mut events = JsonlEvents::to(&mut kept);

        let written = [event(1), event(2)]
            .map(|event| events.append(&event).map_err(|error| error.to_string()));

        assert_eq!(written, [Ok(()), Ok(())]);
        let Kept { bytes, flushes } = kept;
        let text = String::from_utf8(bytes).unwrap_or_default();
        let read: Result<Vec<Event>, _> = text.lines().map(serde_json::from_str).collect();
        assert_eq!(
            read.map_err(|error| error.to_string()),
            Ok(vec![event(1), event(2)])
        );
        let compact = [event(1), event(2)]
            .map(|event| serde_json::to_string(&event).unwrap_or_default() + "\n");
        assert_eq!(text, compact.concat(), "one compact line each");
        assert_eq!(flushes, 2);
    }

    #[test]
    fn a_line_is_written_up_to_its_cap_and_refused_one_byte_over() {
        let base = line_len(&event(0));
        let mut kept = Kept::default();
        let mut events = JsonlEvents::with_caps(Some(&mut kept), base + 10, usize::MAX);

        let at_cap = events.append(&event(10));
        let over = events.append(&event(11));

        assert!(matches!(at_cap, Ok(())), "{at_cap:?}");
        assert!(
            matches!(over, Err(EventWriteError::LineTooLong { bytes }) if bytes == base + 11),
            "{over:?}"
        );
        assert_eq!(
            kept.bytes.len(),
            base + 11,
            "only the line at the cap and its newline"
        );
    }

    #[test]
    fn a_stream_is_written_up_to_its_cap_and_refused_past_it() {
        let line = line_len(&event(0)) + 1;
        let mut kept = Kept::default();
        let mut events = JsonlEvents::with_caps(Some(&mut kept), usize::MAX, line * 2);

        let first = events.append(&event(0));
        let at_cap = events.append(&event(0));
        let past = events.append(&event(0));

        assert!(
            matches!((&first, &at_cap), (Ok(()), Ok(()))),
            "{first:?} {at_cap:?}"
        );
        assert!(matches!(past, Err(EventWriteError::StreamFull)), "{past:?}");
        assert_eq!(kept.bytes.len(), line * 2, "nothing of the refused line");
    }

    /// A writer whose reader is gone.
    struct Closed;

    impl Write for Closed {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_stream_that_cannot_be_written_is_an_error() {
        let mut events = JsonlEvents::to(Closed);

        let result = events.append(&event(0));

        assert!(
            matches!(&result, Err(EventWriteError::Write(error)) if error.kind() == io::ErrorKind::BrokenPipe),
            "{result:?}"
        );
    }

    #[test]
    fn without_a_stream_every_event_is_dropped() {
        let mut events = JsonlEvents::<Closed>::none();

        let result = events.append(&event(MAX_LINE_OVER));

        assert!(matches!(result, Ok(())), "{result:?}");
    }

    /// Longer than any cap's line.
    const MAX_LINE_OVER: usize = super::MAX_EVENT_LINE_BYTES + 1;
}
