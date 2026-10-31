//! The upstream jobs' results and outputs, as `ASSAY_NEEDS=${{ toJSON(needs) }}` hands them
//! to a job (03 §4a): read strictly, so a job never acts on a shape it does not know (I6).

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use serde_json::Value;

use super::OutputName;
use super::shown;

/// The longest `ASSAY_NEEDS` read, in bytes (256 KiB).
///
/// It is above the 128 KiB Linux accepts for one environment string (`MAX_ARG_STRLEN`), so
/// no `needs` a job can be given is refused for its size, while the parse stays bounded.
pub const MAX_NEEDS_BYTES: usize = 256 << 10;

/// A job of the pipeline another job may need, by its id in the workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum JobName {
    /// `guard`.
    Guard,
    /// `open`.
    Open,
    /// `plan`.
    Plan,
    /// `gates-static`.
    GatesStatic,
    /// `gates-lint`.
    GatesLint,
    /// `gates-test`.
    GatesTest,
    /// `admit`.
    Admit,
    /// `review`.
    Review,
    /// `release`.
    Release,
}

impl JobName {
    /// Every job of the pipeline another may need.
    pub const ALL: [Self; 9] = [
        Self::Guard,
        Self::Open,
        Self::Plan,
        Self::GatesStatic,
        Self::GatesLint,
        Self::GatesTest,
        Self::Admit,
        Self::Review,
        Self::Release,
    ];

    /// The job's id in the workflow.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Guard => "guard",
            Self::Open => "open",
            Self::Plan => "plan",
            Self::GatesStatic => "gates-static",
            Self::GatesLint => "gates-lint",
            Self::GatesTest => "gates-test",
            Self::Admit => "admit",
            Self::Review => "review",
            Self::Release => "release",
        }
    }

    /// The job whose id is `id`.
    fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|job| job.as_str() == id)
    }
}

impl fmt::Display for JobName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How an upstream job ended, as `needs.<job>.result` says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JobResult {
    /// `success`.
    Success,
    /// `failure`.
    Failure,
    /// `cancelled`.
    Cancelled,
    /// `skipped`.
    Skipped,
}

impl JobResult {
    /// The result as GitHub writes it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Cancelled => "cancelled",
            Self::Skipped => "skipped",
        }
    }

    /// The result GitHub writes as `word`.
    fn from_word(word: &str) -> Option<Self> {
        [Self::Success, Self::Failure, Self::Cancelled, Self::Skipped]
            .into_iter()
            .find(|result| result.as_str() == word)
    }
}

impl fmt::Display for JobResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One upstream job: its result and the outputs it wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstream {
    result: JobResult,
    outputs: BTreeMap<String, String>,
}

impl Upstream {
    /// How the job ended.
    #[must_use]
    pub const fn result(&self) -> JobResult {
        self.result
    }

    /// The value of output `name`, if the job wrote it.
    #[must_use]
    pub fn output(&self, name: &str) -> Option<&str> {
        self.outputs.get(name).map(String::as_str)
    }

    /// Every output the job wrote, by name.
    pub fn outputs(&self) -> impl Iterator<Item = (&str, &str)> + '_ {
        self.outputs
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
    }

    /// The entry of `job` in `needs`: exactly `result` and `outputs`, every output a string.
    fn from_value(job: JobName, value: Value) -> Result<Self, NeedsError> {
        let entry = |problem| NeedsError::Entry { job, problem };
        let Value::Object(mut fields) = value else {
            return Err(entry(EntryProblem::NotAnObject));
        };
        let result = fields
            .remove("result")
            .ok_or_else(|| entry(EntryProblem::MissingResult))?;
        let outputs = fields
            .remove("outputs")
            .ok_or_else(|| entry(EntryProblem::MissingOutputs))?;
        if let Some(key) = fields.keys().next() {
            return Err(entry(EntryProblem::UnknownKey(shown(key))));
        }
        let result = result
            .as_str()
            .and_then(JobResult::from_word)
            .ok_or(NeedsError::Result { job })?;
        let Value::Object(outputs) = outputs else {
            return Err(entry(EntryProblem::OutputsNotAnObject));
        };
        let outputs = outputs
            .into_iter()
            .map(|(name, value)| match value {
                Value::String(value) => Ok((name, value)),
                _ => Err(NeedsError::NotString {
                    job,
                    name: shown(&name),
                }),
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { result, outputs })
    }
}

/// The upstream jobs a job needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Needs(BTreeMap<JobName, Upstream>);

impl Needs {
    /// Reads `toJSON(needs)` from `bytes`: an object with one entry per job in `expected`
    /// and no other, each `{"result": <result>, "outputs": {<name>: <string>}}`.
    ///
    /// ```
    /// use assay_cli::{JobName, JobResult, Needs};
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let needs = Needs::parse(
    ///     br#"{"guard": {"result": "success", "outputs": {"pr": "7"}}}"#,
    ///     &[JobName::Guard],
    /// )?;
    /// let guard = needs.job(JobName::Guard).ok_or("guard is needed")?;
    /// assert_eq!((guard.result(), guard.output("pr")), (JobResult::Success, Some("7")));
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// [`NeedsError::TooLarge`] past [`MAX_NEEDS_BYTES`], unparsed; [`NeedsError::Parse`] for
    /// text that is not one JSON value or repeats a key; [`NeedsError::NotAnObject`],
    /// [`NeedsError::UnexpectedJob`], [`NeedsError::MissingJob`], [`NeedsError::Entry`],
    /// [`NeedsError::Result`] and [`NeedsError::NotString`] for any other shape.
    pub fn parse(bytes: &[u8], expected: &[JobName]) -> Result<Self, NeedsError> {
        if bytes.len() > MAX_NEEDS_BYTES {
            return Err(NeedsError::TooLarge { size: bytes.len() });
        }
        let value = assay_app::parse_json_strict(bytes).map_err(NeedsError::Parse)?;
        let Value::Object(entries) = value else {
            return Err(NeedsError::NotAnObject);
        };
        let mut jobs = BTreeMap::new();
        for (id, entry) in entries {
            let job = JobName::from_id(&id)
                .filter(|job| expected.contains(job))
                .ok_or_else(|| NeedsError::UnexpectedJob { name: shown(&id) })?;
            jobs.insert(job, Upstream::from_value(job, entry)?);
        }
        if let Some(&job) = expected.iter().find(|job| !jobs.contains_key(job)) {
            return Err(NeedsError::MissingJob { job });
        }
        Ok(Self(jobs))
    }

    /// Upstream job `job`, if it is needed.
    #[must_use]
    pub fn job(&self, job: JobName) -> Option<&Upstream> {
        self.0.get(&job)
    }
}

/// What is wrong with one job's entry in `needs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryProblem {
    /// The entry is not an object.
    NotAnObject,
    /// The entry has a key other than `result` and `outputs` (its first 64 bytes).
    UnknownKey(String),
    /// The entry has no `result`.
    MissingResult,
    /// The entry has no `outputs`.
    MissingOutputs,
    /// `outputs` is not an object.
    OutputsNotAnObject,
}

impl fmt::Display for EntryProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAnObject => f.write_str("is not an object"),
            Self::UnknownKey(key) => write!(f, "has unknown key {key:?}"),
            Self::MissingResult => f.write_str("has no `result`"),
            Self::MissingOutputs => f.write_str("has no `outputs`"),
            Self::OutputsNotAnObject => f.write_str("has `outputs` that are not an object"),
        }
    }
}

/// Why `ASSAY_NEEDS`, or the outputs read from it, was refused (I6).
#[derive(Debug)]
pub enum NeedsError {
    /// The text is over [`MAX_NEEDS_BYTES`]; it was not parsed.
    TooLarge {
        /// Its length in bytes.
        size: usize,
    },
    /// The text is not one JSON value, or an object in it repeats a key.
    Parse(serde_json::Error),
    /// The value is not an object.
    NotAnObject,
    /// A job that is not needed, or not a pipeline job (its id's first 64 bytes).
    UnexpectedJob {
        /// The job's id.
        name: String,
    },
    /// A needed job is absent.
    MissingJob {
        /// The job.
        job: JobName,
    },
    /// A job's entry is not `{result, outputs}`.
    Entry {
        /// The job.
        job: JobName,
        /// What is wrong.
        problem: EntryProblem,
    },
    /// A job's `result` is not `success`, `failure`, `cancelled` or `skipped`.
    Result {
        /// The job.
        job: JobName,
    },
    /// An output's value is not a string (its name's first 64 bytes).
    NotString {
        /// The job.
        job: JobName,
        /// The output.
        name: String,
    },
    /// The job whose outputs are read did not succeed.
    NotSucceeded {
        /// The job.
        job: JobName,
        /// How it ended.
        result: JobResult,
    },
    /// The job wrote an output that is not one of its own (its name's first 64 bytes).
    UnexpectedOutput {
        /// The job.
        job: JobName,
        /// The output.
        name: String,
    },
    /// A required output is absent or empty.
    MissingOutput {
        /// The job.
        job: JobName,
        /// The output.
        name: OutputName,
    },
    /// An output's value does not parse as what the output carries, or is written where the
    /// other outputs say it must not be.
    Output {
        /// The job.
        job: JobName,
        /// The output.
        name: OutputName,
    },
}

impl fmt::Display for NeedsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { size } => write!(
                f,
                "ASSAY_NEEDS is {size} bytes, over the {MAX_NEEDS_BYTES}-byte cap"
            ),
            Self::Parse(_) => {
                f.write_str("ASSAY_NEEDS is not one JSON value without repeated keys")
            }
            Self::NotAnObject => f.write_str("ASSAY_NEEDS is not a JSON object"),
            Self::UnexpectedJob { name } => write!(
                f,
                "ASSAY_NEEDS names job {name:?}, which this job does not need"
            ),
            Self::MissingJob { job } => write!(f, "ASSAY_NEEDS has no entry for job {job}"),
            Self::Entry { job, problem } => write!(f, "ASSAY_NEEDS' entry for {job} {problem}"),
            Self::Result { job } => {
                write!(f, "ASSAY_NEEDS' result for {job} is not a job result")
            }
            Self::NotString { job, name } => {
                write!(f, "output {name:?} of {job} is not a string")
            }
            Self::NotSucceeded { job, result } => {
                write!(
                    f,
                    "job {job} ended with {result}, so its outputs are not read"
                )
            }
            Self::UnexpectedOutput { job, name } => write!(
                f,
                "job {job} wrote output {name:?}, which is not one of its own"
            ),
            Self::MissingOutput { job, name } => {
                write!(f, "job {job} did not write output {name}")
            }
            Self::Output { job, name } => write!(f, "output {name} of job {job} is not valid"),
        }
    }
}

impl Error for NeedsError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Parse(source) => Some(source),
            Self::TooLarge { .. }
            | Self::NotAnObject
            | Self::UnexpectedJob { .. }
            | Self::MissingJob { .. }
            | Self::Entry { .. }
            | Self::Result { .. }
            | Self::NotString { .. }
            | Self::NotSucceeded { .. }
            | Self::UnexpectedOutput { .. }
            | Self::MissingOutput { .. }
            | Self::Output { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests;
