//! The GitHub Actions plumbing every pipeline command shares (#60): the run it belongs to
//! ([`RunEnv`], with the first-attempt guard of R3-05), the upstream jobs' results and outputs
//! as `ASSAY_NEEDS=${{ toJSON(needs) }}` hands them over ([`Needs`]), and the job's own outputs
//! written to `GITHUB_OUTPUT` ([`Outputs`]). The encoding of each job's outputs is
//! [`crate::outputs`]'s; 03 §4a documents the channel. A job's `--out` directory is an
//! [`OutDir`].

mod needs;
mod out_dir;
mod output;
mod run_env;

pub use needs::{EntryProblem, JobName, JobResult, MAX_NEEDS_BYTES, Needs, NeedsError, Upstream};
pub use out_dir::{OutDir, OutDirError};
pub use output::{OutputError, OutputName, Outputs};
pub(crate) use run_env::canonical_positive;
pub use run_env::{FirstAttempt, NotFirstAttempt, RunEnv, RunEnvError, RunVariables};

/// At most this many bytes of a name read from outside are kept in an error.
const NAME_SHOWN: usize = 64;

/// `name`, cut at a character boundary to [`NAME_SHOWN`] bytes, for an error message.
pub(crate) fn shown(name: &str) -> String {
    name.char_indices()
        .take_while(|&(start, char)| start.saturating_add(char.len_utf8()) <= NAME_SHOWN)
        .map(|(_, char)| char)
        .collect()
}
