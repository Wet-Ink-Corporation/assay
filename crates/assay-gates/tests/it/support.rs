//! A scripted process supervisor and fixture configuration for the exec adapter tests.

use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use assay_app::{Plugin, PluginsConfig, Termination};
use assay_gates::{ProcessSpec, Supervisor};

pub(crate) const GIT: &str = "0123456789abcdef0123456789abcdef01234567";
const HEX64: &str = "dc7c1d8e3ccab68cfdb828ce40a446c3913f7172f899ffe3f439839cfd917a8b";

/// An exec gate `custom-lint` in `gates-lint` with a 90 s timeout, and a builtin temper.
pub(crate) fn config() -> Result<PluginsConfig, Box<dyn Error>> {
    Ok(format!(
        r#"
[assay]
digest = "sha256:{HEX64}"

[[plugin]]
name = "temper"
adapter = "builtin:temper"
pin = "git:{GIT}"
required = true

[[plugin]]
name = "custom-lint"
adapter = "exec"
exec = ["./scripts/lint.sh", "--json"]
job = "lint"
required = true
timeout_s = 90
"#
    )
    .parse()?)
}

/// The plugin named `name` in [`config`].
pub(crate) fn plugin(config: &PluginsConfig, name: &str) -> Result<Plugin, Box<dyn Error>> {
    config
        .plugins()
        .iter()
        .find(|plugin| plugin.name().as_str() == name)
        .cloned()
        .ok_or_else(|| format!("no plugin {name}").into())
}

/// A gate document from `name` concluding `conclusion`.
pub(crate) fn document(name: &str, conclusion: &str) -> Vec<u8> {
    format!(
        r#"{{"schema":"assay.findings/1","source":{{"kind":"gate","name":"{name}","version":"v","run_key":"k"}},"conclusion":"{conclusion}","findings":[]}}"#
    )
    .into_bytes()
}

/// What the supervisor was asked to run, owned so a test can inspect it afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SeenSpec {
    pub(crate) program: PathBuf,
    pub(crate) args: Vec<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) env: Vec<(OsString, OsString)>,
    pub(crate) timeout: Duration,
    pub(crate) max_output_bytes: usize,
}

/// What the scripted "gate process" leaves at the findings path.
#[derive(Debug, Clone)]
pub(crate) enum Leaves {
    Nothing,
    File(Vec<u8>),
    SymlinkTo(PathBuf),
    /// The process never starts: the supervisor fails.
    CannotStart,
}

/// Why the fake "process" could not play its part.
#[derive(Debug)]
pub(crate) enum FakeSpawnError {
    NoFindingsPath,
    Write(io::Error),
    CannotStart,
}

impl fmt::Display for FakeSpawnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CannotStart => f.write_str("scripted start failure"),
            Self::NoFindingsPath => f.write_str("no ASSAY_FINDINGS_OUT in the environment"),
            Self::Write(error) => write!(f, "could not write the findings path: {error}"),
        }
    }
}

impl Error for FakeSpawnError {}

/// Plays the gate process: writes what [`Leaves`] says at `ASSAY_FINDINGS_OUT`, then ends with
/// `termination`.
#[derive(Debug)]
pub(crate) struct FakeSupervisor {
    pub(crate) termination: Termination,
    pub(crate) leaves: Leaves,
    pub(crate) seen: Vec<SeenSpec>,
}

impl FakeSupervisor {
    pub(crate) fn new(termination: Termination, leaves: Leaves) -> Self {
        Self {
            termination,
            leaves,
            seen: Vec::new(),
        }
    }
}

fn findings_path(env: &[(OsString, OsString)]) -> Option<&Path> {
    env.iter()
        .find(|(name, _)| name == "ASSAY_FINDINGS_OUT")
        .map(|(_, value)| Path::new(value))
}

impl Supervisor for FakeSupervisor {
    type Error = FakeSpawnError;

    fn run(&mut self, spec: &ProcessSpec<'_>) -> Result<Termination, FakeSpawnError> {
        self.seen.push(SeenSpec {
            program: spec.program.to_path_buf(),
            args: spec.args.to_vec(),
            cwd: spec.cwd.to_path_buf(),
            env: spec.env.to_vec(),
            timeout: spec.timeout,
            max_output_bytes: spec.max_output_bytes,
        });
        let path = findings_path(spec.env).ok_or(FakeSpawnError::NoFindingsPath)?;
        match &self.leaves {
            Leaves::Nothing => {}
            Leaves::CannotStart => return Err(FakeSpawnError::CannotStart),
            Leaves::File(bytes) => fs::write(path, bytes).map_err(FakeSpawnError::Write)?,
            Leaves::SymlinkTo(target) => {
                std::os::unix::fs::symlink(target, path).map_err(FakeSpawnError::Write)?;
            }
        }
        Ok(self.termination)
    }
}
