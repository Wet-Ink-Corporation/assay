//! A job's outputs, appended to the file `GITHUB_OUTPUT` names as single `name=value` lines
//! (I5): names from a closed set, values that never hold a line break, so no value can forge a
//! second output line.

use std::error::Error;
use std::fmt;
use std::io::{self, Write};

use assay_app::ArtifactJob;

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
    /// guard and decide-only guard: `trigger`, the event behind the run.
    Trigger,
    /// guard: `before`, the head before a `synchronize` push; written for that event only.
    Before,
    /// guard: `default_branch`, the repository's default branch.
    DefaultBranch,
    /// gates-static, gates-lint, gates-test: `gates`, what the job reported for each plugin it
    /// ran, as compact JSON.
    Gates,
    /// review: `stage`, how far the review got; the placeholder writes `not-run` (D11 A).
    Stage,
    /// review: `cost_aeu`, what the review spent.
    CostAeu,
    /// review: `documents`, the review's documents.
    Documents,
    /// plan and admit: `run_digest`, the sha256 of the `run.json` the job wrote (admit's is the
    /// admitted one).
    RunDigest,
    /// plan: `run_key`, the run's evidence key.
    RunKey,
    /// plan and admit: `tier`, the run's tier (admit's is the tier the LLM stage runs at).
    Tier,
    /// plan and admit: `review`, `true` when the run gets an LLM review (admit's: when the
    /// review job runs).
    Review,
    /// plan: `merge_base`, git's merge base of the pinned base and head (D4 A).
    MergeBase,
    /// plan: `artifact_plan`, the plan artifact's name.
    ArtifactPlan,
    /// plan: `artifact_gates_static`.
    ArtifactGatesStatic,
    /// plan: `artifact_gates_lint`.
    ArtifactGatesLint,
    /// plan: `artifact_gates_test`.
    ArtifactGatesTest,
    /// plan: `artifact_admit`.
    ArtifactAdmit,
    /// plan: `artifact_review`.
    ArtifactReview,
    /// plan: `artifact_decide`.
    ArtifactDecide,
    /// admit: `admission`, what the admission decided.
    Admission,
    /// admit: `deferral`, why the LLM stage was deferred; written for a deferral only.
    Deferral,
    /// admit: `slot`, the index of the review slot held; written only when one is held.
    Slot,
    /// admit: `lease_commit`, the commit the slot's ref points at; written with `slot`.
    LeaseCommit,
    /// admit: `reserved_aeu`, what the admission reserved.
    ReservedAeu,
    /// admit: `degrade`, whether and from which tier the admission degraded.
    Degrade,
    /// admit: `pack_digest`, the sha256 of the admitted `context-pack.json`.
    PackDigest,
    /// decide: `bundle`, `true` when it wrote `run-inputs.json` for the upload step.
    Bundle,
    /// decide: `bundle_digest`, the sha256 of the `run-inputs.json` it wrote; written with
    /// `bundle=true` only.
    BundleDigest,
    /// decide-only guard: `prs`, the PRs to decide again, as a compact JSON array (#61 DO-10).
    Prs,
    /// decide-only decide: `artifact`, the name `assay-decision-<pr>-<nonce>` the upload step
    /// stores `run-decision.json` under; written with `bundle=true` only (#61 DO-11).
    Artifact,
}

impl OutputName {
    /// The outputs `decide` writes (#60 ST-14), in order.
    pub const DECIDE: [Self; 2] = [Self::Bundle, Self::BundleDigest];

    /// The outputs `decide-only guard` writes (#61 DO-10), in order; `trigger` only beside a
    /// PR to decide.
    pub const DECIDE_ONLY_GUARD: [Self; 2] = [Self::Prs, Self::Trigger];

    /// The outputs `decide-only decide` writes (#61 DO-11), in order; `bundle_digest` and
    /// `artifact` only with `bundle=true`.
    pub const REDECIDE: [Self; 3] = [Self::Bundle, Self::BundleDigest, Self::Artifact];

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

    /// The outputs `review` writes.
    pub const REVIEW: [Self; 3] = [Self::Stage, Self::CostAeu, Self::Documents];

    /// The outputs `admit` writes (#60 ST-11 writes them; `release` reads `slot` and
    /// `lease_commit`).
    pub const ADMIT: [Self; 10] = [
        Self::Admission,
        Self::Deferral,
        Self::Slot,
        Self::LeaseCommit,
        Self::ReservedAeu,
        Self::Tier,
        Self::Degrade,
        Self::RunDigest,
        Self::PackDigest,
        Self::Review,
    ];

    /// The outputs `plan` writes: the run's facts, then each job's artifact name in
    /// [`ArtifactJob::ALL`]'s order. `draft` is guard's name too.
    pub const PLAN: [Self; 13] = [
        Self::RunDigest,
        Self::RunKey,
        Self::Tier,
        Self::Draft,
        Self::Review,
        Self::MergeBase,
        Self::ArtifactPlan,
        Self::ArtifactGatesStatic,
        Self::ArtifactGatesLint,
        Self::ArtifactGatesTest,
        Self::ArtifactAdmit,
        Self::ArtifactReview,
        Self::ArtifactDecide,
    ];

    /// The output carrying `job`'s artifact name.
    #[must_use]
    pub const fn artifact(job: ArtifactJob) -> Self {
        match job {
            ArtifactJob::Plan => Self::ArtifactPlan,
            ArtifactJob::GatesStatic => Self::ArtifactGatesStatic,
            ArtifactJob::GatesLint => Self::ArtifactGatesLint,
            ArtifactJob::GatesTest => Self::ArtifactGatesTest,
            ArtifactJob::Admit => Self::ArtifactAdmit,
            ArtifactJob::Review => Self::ArtifactReview,
            ArtifactJob::Decide => Self::ArtifactDecide,
        }
    }

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
            Self::Stage => "stage",
            Self::CostAeu => "cost_aeu",
            Self::Documents => "documents",
            Self::RunDigest => "run_digest",
            Self::RunKey => "run_key",
            Self::Tier => "tier",
            Self::Review => "review",
            Self::MergeBase => "merge_base",
            Self::ArtifactPlan => "artifact_plan",
            Self::ArtifactGatesStatic => "artifact_gates_static",
            Self::ArtifactGatesLint => "artifact_gates_lint",
            Self::ArtifactGatesTest => "artifact_gates_test",
            Self::ArtifactAdmit => "artifact_admit",
            Self::ArtifactReview => "artifact_review",
            Self::ArtifactDecide => "artifact_decide",
            Self::Admission => "admission",
            Self::Deferral => "deferral",
            Self::Slot => "slot",
            Self::LeaseCommit => "lease_commit",
            Self::ReservedAeu => "reserved_aeu",
            Self::Degrade => "degrade",
            Self::PackDigest => "pack_digest",
            Self::Bundle => "bundle",
            Self::BundleDigest => "bundle_digest",
            Self::Prs => "prs",
            Self::Artifact => "artifact",
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
    fn review_writes_stage_cost_and_documents() {
        let names = OutputName::REVIEW.map(OutputName::as_str);

        assert_eq!(names, ["stage", "cost_aeu", "documents"]);
    }

    #[test]
    fn admit_writes_the_names_the_workflow_reads() {
        let names = OutputName::ADMIT.map(OutputName::as_str);

        assert_eq!(
            names,
            [
                "admission",
                "deferral",
                "slot",
                "lease_commit",
                "reserved_aeu",
                "tier",
                "degrade",
                "run_digest",
                "pack_digest",
                "review",
            ]
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

    #[test]
    fn plan_writes_the_runs_facts_then_one_artifact_name_per_job() {
        let names = OutputName::PLAN.map(OutputName::as_str);

        assert_eq!(
            names,
            [
                "run_digest",
                "run_key",
                "tier",
                "draft",
                "review",
                "merge_base",
                "artifact_plan",
                "artifact_gates_static",
                "artifact_gates_lint",
                "artifact_gates_test",
                "artifact_admit",
                "artifact_review",
                "artifact_decide",
            ],
            "03 §4a and the workflow's `needs.plan.outputs.<name>` read these names"
        );
    }

    #[test]
    fn each_artifact_jobs_output_is_named_after_the_job() {
        for job in ArtifactJob::ALL {
            let name = OutputName::artifact(job).as_str();

            assert_eq!(
                name,
                format!("artifact_{}", job.name().replace('-', "_")),
                "{job:?}"
            );
        }
    }
}
