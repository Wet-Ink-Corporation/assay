//! `assay review --placeholder` (#60 ST-9, D11 A): the Z3 review job until the real review
//! (M2) exists. It appends [`REVIEW_PLACEHOLDER`] (`stage=not-run`, `cost_aeu=0`,
//! `documents=[]`) to the file `GITHUB_OUTPUT` names, so `decide` applies the LLM-unavailable
//! policy. It reads no secret, no run variable and no file but `GITHUB_OUTPUT`.

use std::ffi::OsString;
use std::fmt;
use std::io;

use assay_cli::outputs::REVIEW_PLACEHOLDER;
use assay_cli::{OutputError, Outputs};

use crate::doctor::write_chain;
use crate::guard_job::{OUTPUT, open_output};

/// The one argument the placeholder takes.
const PLACEHOLDER: &str = "--placeholder";

/// Why `assay review` could not write its outputs.
#[derive(Debug)]
pub(crate) enum ReviewCliError {
    /// It was not given exactly `--placeholder`.
    Usage,
    /// `GITHUB_OUTPUT` is not set.
    OutputVariable,
    /// `GITHUB_OUTPUT` could not be opened for appending, or is not a regular file.
    OutputFile(io::Error),
    /// An output could not be written.
    Output(OutputError),
}

impl fmt::Display for ReviewCliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage => write!(
                f,
                "usage: assay review {PLACEHOLDER} (the real review is not built yet)"
            ),
            Self::OutputVariable => write!(f, "{OUTPUT} is not set"),
            Self::OutputFile(source) => write!(f, "opening {OUTPUT}: {source}"),
            Self::Output(source) => write_chain(f, source),
        }
    }
}

/// Runs `assay review` with `args` (exactly `--placeholder`), appending the placeholder's
/// outputs to `output`, the file `GITHUB_OUTPUT` names.
///
/// # Errors
///
/// [`ReviewCliError::Usage`] for any other arguments, before `GITHUB_OUTPUT` is read; the
/// other variants when `GITHUB_OUTPUT` is unset, is not a regular file that can be opened for
/// appending (it is never created), or a line cannot be written.
pub(crate) fn run(
    mut args: impl Iterator<Item = OsString>,
    output: Option<OsString>,
) -> Result<(), ReviewCliError> {
    let placeholder = args.next().is_some_and(|arg| arg == PLACEHOLDER);
    if !placeholder || args.next().is_some() {
        return Err(ReviewCliError::Usage);
    }
    let path = output.ok_or(ReviewCliError::OutputVariable)?;
    let file = open_output(path).map_err(ReviewCliError::OutputFile)?;
    let mut outputs = Outputs::new(file);
    for (name, value) in REVIEW_PLACEHOLDER {
        outputs.write(name, value).map_err(ReviewCliError::Output)?;
    }
    Ok(())
}
