//! Fixtures and a model call to run them under.

use std::cell::RefCell;
use std::error::Error;
use std::fs;
use std::io;
use std::num::NonZeroU8;
use std::path::{Path, PathBuf};
use std::time::Duration;

use assay_app::{ModelCall, Prompt};
use assay_domain::Aeu;
use assay_runner::GroupKill;

/// One kill a [`RecordingKill`] was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Asked {
    /// [`GroupKill::kill_led_group`], with whether the leader still held its pid (was not yet
    /// reaped) at that moment.
    Led { group: u32, leader_unreaped: bool },
    /// [`GroupKill::kill_group`], the sweep after a reaped leader.
    Sweep { group: u32 },
}

/// A [`GroupKill`] that signals nothing: it records each kill it is asked for, and fails when
/// told to.
#[derive(Debug, Default)]
pub(crate) struct RecordingKill {
    fail: bool,
    // Interior mutability: `GroupKill`'s methods take `&self`, as `ModelRunner::run` does.
    asked: RefCell<Vec<Asked>>,
}

impl RecordingKill {
    /// A killer whose every kill fails, as when the group outlives its grace.
    pub(crate) fn failing() -> Self {
        Self {
            fail: true,
            asked: RefCell::default(),
        }
    }

    /// Each kill asked for since the last call.
    pub(crate) fn take_asked(&self) -> Vec<Asked> {
        self.asked.take()
    }

    fn record(&self, asked: Asked) -> io::Result<()> {
        self.asked.borrow_mut().push(asked);
        if self.fail {
            Err(io::Error::other("scripted: the group is still alive"))
        } else {
            Ok(())
        }
    }
}

impl GroupKill for RecordingKill {
    fn kill_group(&self, group: u32) -> io::Result<()> {
        self.record(Asked::Sweep { group })
    }

    fn kill_led_group(&self, group: u32) -> io::Result<()> {
        let leader_unreaped = Path::new(&format!("/proc/{group}")).exists();
        self.record(Asked::Led {
            group,
            leader_unreaped,
        })
    }
}

/// The cap every test call runs under: 0.74 AEU, the lite tier's.
pub(crate) const BUDGET: Aeu = Aeu::from_micros(740_000);

/// The directory holding this crate's test fixtures.
pub(crate) fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// The path of the recorded stream `name` (`tests/fixtures/stream/<name>.jsonl`).
pub(crate) fn stream_path(name: &str) -> PathBuf {
    fixtures_dir().join("stream").join(format!("{name}.jsonl"))
}

/// The recorded stream `name`.
pub(crate) fn stream(name: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    Ok(fs::read(stream_path(name))?)
}

/// A call with fixed, absolute paths, the lite budget and a ten-minute timeout.
pub(crate) fn call() -> Result<ModelCall, Box<dyn Error>> {
    Ok(ModelCall {
        system_prompt_file: PathBuf::from("/bundle/system.md"),
        user_prompt: Prompt::new("Context pack, then the role instruction.")?,
        json_schema: r#"{"type":"object"}"#.to_owned(),
        add_dir: PathBuf::from("/work/pr"),
        max_turns: NonZeroU8::new(8).ok_or("8 is not zero")?,
        budget: BUDGET,
        timeout: Duration::from_secs(600),
    })
}
