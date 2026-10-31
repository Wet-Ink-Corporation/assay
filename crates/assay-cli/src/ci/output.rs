//! A job's outputs, appended to the file `GITHUB_OUTPUT` names as single `name=value` lines
//! (I5): names from a closed set, values that never hold a line break, so no value can forge a
//! second output line.

use std::error::Error;
use std::fmt;
use std::io::{self, Write};

/// The outputs a pipeline job writes, by the job that writes them. Each story adds its job's
/// names (03 §4a).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OutputName {
    /// guard: `run`, `true` when the pipeline runs for the event, else `false`.
    Run,
    /// guard: `pr`, the PR number.
    Pr,
    /// guard: `head_sha`.
    HeadSha,
    /// guard: `base_sha`.
    BaseSha,
    /// guard: `base_ref`, the base branch.
    BaseRef,
    /// guard: `draft`, `true` or `false`.
    Draft,
    /// guard: `author_id`, the PR author's user id.
    AuthorId,
    /// guard: `sender_id`, the event sender's user id.
    SenderId,
    /// guard: `trigger`, the event behind the run.
    Trigger,
    /// guard: `before`, the head before a `synchronize` push; written for that event only.
    Before,
    /// guard: `default_branch`, the repository's default branch.
    DefaultBranch,
    /// gates-static, gates-lint, gates-test: `gates`, what the job reported for each plugin it
    /// ran, as compact JSON.
    Gates,
}

impl OutputName {
    /// The outputs `guard` writes.
    pub const GUARD: [Self; 11] = [
        Self::Run,
        Self::Pr,
        Self::HeadSha,
        Self::BaseSha,
        Self::BaseRef,
        Self::Draft,
        Self::AuthorId,
        Self::SenderId,
        Self::Trigger,
        Self::Before,
        Self::DefaultBranch,
    ];

    /// The outputs each gates job writes.
    pub const GATES: [Self; 1] = [Self::Gates];

    /// The name as the workflow reads it, `needs.<job>.outputs.<name>`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Pr => "pr",
            Self::HeadSha => "head_sha",
            Self::BaseSha => "base_sha",
            Self::BaseRef => "base_ref",
            Self::Draft => "draft",
            Self::AuthorId => "author_id",
            Self::SenderId => "sender_id",
            Self::Trigger => "trigger",
            Self::Before => "before",
            Self::DefaultBranch => "default_branch",
            Self::Gates => "gates",
        }
    }
}

impl fmt::Display for OutputName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why an output was not written.
#[derive(Debug)]
pub enum OutputError {
    /// The value holds a line feed or a carriage return; nothing was written.
    Multiline {
        /// The output.
        name: OutputName,
    },
    /// Writing the line failed; part of it may be in the file.
    Write {
        /// The output.
        name: OutputName,
        /// The failure.
        source: io::Error,
    },
}

impl fmt::Display for OutputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Multiline { name } => write!(
                f,
                "output {name} was not written: its value holds a line break"
            ),
            Self::Write { name, .. } => write!(f, "writing output {name} failed"),
        }
    }
}

impl Error for OutputError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Multiline { .. } => None,
            Self::Write { source, .. } => Some(source),
        }
    }
}

/// Appends a job's outputs to `out`, the file `GITHUB_OUTPUT` names, which the caller opened
/// for appending.
#[derive(Debug)]
pub struct Outputs<W> {
    out: W,
}

impl<W: Write> Outputs<W> {
    /// Outputs written to `out`; pass `&mut file` to keep the file.
    pub const fn new(out: W) -> Self {
        Self { out }
    }

    /// Appends `name=value` and a line feed in one write.
    ///
    /// ```
    /// use assay_cli::{OutputName, Outputs};
    ///
    /// let mut file = Vec::new();
    /// Outputs::new(&mut file).write(OutputName::Run, "true")?;
    /// assert_eq!(file, b"run=true\n");
    /// # Ok::<(), assay_cli::OutputError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// [`OutputError::Multiline`] for a value holding `\n` or `\r`, with nothing written;
    /// [`OutputError::Write`] when the write fails.
    pub fn write(&mut self, name: OutputName, value: &str) -> Result<(), OutputError> {
        if value.contains(['\n', '\r']) {
            return Err(OutputError::Multiline { name });
        }
        let line = format!("{name}={value}\n");
        self.out
            .write_all(line.as_bytes())
            .and_then(|()| self.out.flush())
            .map_err(|source| OutputError::Write { name, source })
    }
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use super::*;

    #[test]
    fn appends_one_name_value_line() {
        let mut file = b"earlier=1\n".to_vec();

        Outputs::new(&mut file)
            .write(OutputName::Run, "true")
            .expect("a single-line value");

        assert_eq!(file, b"earlier=1\nrun=true\n");
    }

    #[test]
    fn a_value_with_a_line_break_writes_nothing() {
        for value in ["a\nrun=true", "a\rb", "\n", "a\r\n"] {
            let mut file = Vec::new();

            let written = Outputs::new(&mut file).write(OutputName::BaseRef, value);

            assert_matches!(
                written,
                Err(OutputError::Multiline {
                    name: OutputName::BaseRef
                }),
                "{value:?}"
            );
            assert_eq!(file, b"", "{value:?}");
        }
    }

    #[test]
    fn an_empty_value_is_written_as_empty() {
        let mut file = Vec::new();

        Outputs::new(&mut file)
            .write(OutputName::Before, "")
            .expect("an empty value");

        assert_eq!(file, b"before=\n");
    }

    #[test]
    fn a_failed_write_is_reported_with_the_output() {
        struct Full;
        impl Write for Full {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::from(io::ErrorKind::StorageFull))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let written = Outputs::new(Full).write(OutputName::Pr, "7");

        assert_matches!(
            written,
            Err(OutputError::Write { name: OutputName::Pr, source })
                if source.kind() == io::ErrorKind::StorageFull
        );
    }

    #[test]
    fn guard_names_are_distinct_snake_case() {
        let mut names: Vec<&str> = OutputName::GUARD.iter().map(|name| name.as_str()).collect();
        names.sort_unstable();
        names.dedup();

        assert_eq!(names.len(), OutputName::GUARD.len());
        assert!(
            names
                .iter()
                .all(|name| name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')),
            "{names:?}"
        );
    }
}
