//! The `Replay` adapter: recorded stream-json transcripts instead of a live model (03 §7).

use std::cell::Cell;

use assay_app::{ModelCall, ModelResult, ModelRunner};

use crate::stream::{self, Ending};

/// A recorded model call: its stream-json transcript and how the process stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recording {
    /// The process exited on its own after writing this transcript.
    Completed(Vec<u8>),
    /// The process was killed at its wall-clock limit after writing this much.
    TimedOut(Vec<u8>),
}

/// A [`ModelRunner`] that answers each call with the next [`Recording`], classified exactly as
/// `ClaudeHeadless` classifies a live transcript, with the call's budget as the cap.
///
/// It keeps its position in a [`Cell`], so it is not `Sync`: one replay serves one thread.
#[derive(Debug)]
pub struct Replay {
    recordings: Vec<Recording>,
    next: Cell<usize>,
}

/// Why a replayed call failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReplayError {
    /// Every recording has been replayed.
    #[error("all {recorded} recorded model calls have been replayed")]
    Exhausted {
        /// How many recordings the replay held.
        recorded: usize,
    },
}

impl Replay {
    /// A replay of `recordings`, in order.
    #[must_use]
    pub fn new(recordings: impl IntoIterator<Item = Recording>) -> Self {
        Self {
            recordings: recordings.into_iter().collect(),
            next: Cell::new(0),
        }
    }
}

impl ModelRunner for Replay {
    type Error = ReplayError;

    /// Classifies the next recording under `call`'s budget.
    ///
    /// # Errors
    /// Returns [`ReplayError::Exhausted`] once every recording has been replayed.
    fn run(&self, call: &ModelCall) -> Result<ModelResult, ReplayError> {
        let index = self.next.get();
        let recording = self.recordings.get(index).ok_or(ReplayError::Exhausted {
            recorded: self.recordings.len(),
        })?;
        self.next.set(index.saturating_add(1));
        let (transcript, ending) = match recording {
            Recording::Completed(transcript) => (transcript, Ending::Exited),
            Recording::TimedOut(transcript) => (transcript, Ending::TimedOut),
        };
        Ok(stream::classify(transcript, ending, call.budget))
    }
}
