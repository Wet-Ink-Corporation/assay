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

/// A [`GroupKill`] that signals nothing: it records each group it is asked to kill, with
/// whether the group's leader still held its pid (was not yet reaped) at that moment, and
/// fails when told to.
#[derive(Debug, Default)]
pub(crate) struct RecordingKill {
    fail: bool,
    // Interior mutability: `GroupKill::kill_group` takes `&self`, as `ModelRunner::run` does.
    asked: RefCell<Vec<(u32, bool)>>,
}

impl RecordingKill {
    /// A killer whose every kill fails, as when the group outlives its grace.
    pub(crate) fn failing() -> Self {
        Self {
            fail: true,
            asked: RefCell::default(),
        }
    }

    /// Each group asked for since the last call, with whether its leader was still unreaped.
    pub(crate) fn take_asked(&self) -> Vec<(u32, bool)> {
        self.asked.take()
    }
}

impl GroupKill for RecordingKill {
    fn kill_group(&self, group: u32) -> io::Result<()> {
        let leader_unreaped = Path::new(&format!("/proc/{group}")).exists();
        self.asked.borrow_mut().push((group, leader_unreaped));
        if self.fail {
            Err(io::Error::other("scripted: the group is still alive"))
        } else {
            Ok(())
        }
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
