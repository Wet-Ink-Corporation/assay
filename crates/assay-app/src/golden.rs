//! The `assay-self` gate (`docs/architecture/02-architecture.md` §7, `05-security.md` T16):
//! `decide` replayed over the golden corpus, byte for byte against the recorded snapshots.
//!
//! The replay runs at the PR's head, so a change to the rubric or to `decide` that moves any
//! verdict fails the gate unless the same PR updates the snapshots; an updated snapshot is a
//! trust-path change in the assay repository (`.assay/globs.toml`), so rubric rule 4 holds it
//! until an owner approves. The caller reads the files; this module only judges them, and
//! everything it cannot check is a failing finding, never a pass (wi-b5affa).

mod report;

use std::collections::BTreeMap;
use std::mem;

use assay_domain::{
    DecisionInputs, Finding, FindingsDocument, FindingsSchema, GateConclusion, Rubric, Source,
    decide,
};

use self::report::Problem;

/// The plugin name of the gate, which its document's `source.name` must carry.
pub const SELF_GATE: &str = "assay-self";

/// The one file allowed directly in the corpus directory besides the cases.
const README: &str = "README.md";
/// A case's `DecisionInputs`, in their serde form.
const INPUTS: &str = "inputs.json";
/// A case's recorded decision: pretty JSON with a final newline.
const DECISION: &str = "decision.json";

/// One entry under the corpus directory, as the caller read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusEntry {
    /// The path relative to the corpus directory, `/`-separated (`rule-9-success/inputs.json`).
    pub path: String,
    /// What the entry is.
    pub kind: CorpusEntryKind,
}

/// What a corpus entry is. Only directories and regular files belong in a corpus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorpusEntryKind {
    /// A regular file and its bytes.
    File(Vec<u8>),
    /// A directory; its own entries are listed separately.
    Directory,
    /// Anything else (a symlink, a FIFO), never followed or read.
    Other,
}

/// What the gate replays: the corpus and the rubric, with the repository paths findings name.
#[derive(Debug, Clone, Copy)]
pub struct GoldenReplay<'a> {
    /// The corpus directory, relative to the repository root (`corpus/golden`).
    pub corpus_dir: &'a str,
    /// Every entry under it, at any depth.
    pub entries: &'a [CorpusEntry],
    /// The rubric file, relative to the repository root (`rubric/v1.toml`).
    pub rubric_path: &'a str,
    /// The rubric file's bytes.
    pub rubric: &'a [u8],
}

/// Replays `decide` over every case of the corpus and reports the result as the gate's
/// `assay.findings/1` document, under `source`.
///
/// The conclusion is `pass` exactly when there is no finding. Each finding is a `blocker`:
/// `golden-mismatch` (a case decides to other bytes than its `decision.json`, at the first
/// differing line), `golden-inputs-invalid` (its `inputs.json` is not `DecisionInputs`),
/// `golden-case-incomplete` (a case lacks one of the two files), `golden-unexpected-entry`
/// (anything but a root `README.md`, case directories and the two regular files in each),
/// `golden-corpus-empty` (no case at all) and `golden-rubric-invalid` (the rubric does not
/// parse, which is then the only finding). Findings are in path order.
///
/// # Examples
///
/// ```
/// use assay_app::{GoldenReplay, SELF_GATE, replay_golden};
/// use assay_domain::{GateConclusion, Source, SourceKind};
///
/// let source = Source {
///     kind: SourceKind::Gate,
///     name: SELF_GATE.to_owned(),
///     version: "0.1.0".to_owned(),
///     role: None,
///     run_key: "sha256:0".to_owned(),
/// };
/// let replay = GoldenReplay {
///     corpus_dir: "corpus/golden",
///     entries: &[],
///     rubric_path: "rubric/v1.toml",
///     rubric: b"version = \"v1\"\nmajors_block_threshold = 2\nmax_requeues = 3\nmicro_roles = []\n",
/// };
///
/// let document = replay_golden(&replay, source);
///
/// // An empty corpus checks nothing, so it fails.
/// assert_eq!(document.conclusion, Some(GateConclusion::Fail));
/// ```
#[must_use]
pub fn replay_golden(replay: &GoldenReplay<'_>, source: Source) -> FindingsDocument {
    let findings = findings(replay);
    let conclusion = if findings.is_empty() {
        GateConclusion::Pass
    } else {
        GateConclusion::Fail
    };
    FindingsDocument {
        schema: FindingsSchema::V1,
        source,
        conclusion: Some(conclusion),
        findings,
        // An `exec` gate's attestation is never believed (#153), so the replay makes none.
        failed_steps: None,
        stats: None,
    }
}

/// Every finding of the replay, in path order.
fn findings(replay: &GoldenReplay<'_>) -> Vec<Finding> {
    let rubric = match parse_rubric(replay.rubric) {
        Ok(rubric) => rubric,
        Err(line) => {
            let problem = Problem::RubricInvalid { line };
            return vec![problem.finding(replay.rubric_path, Some(replay.rubric))];
        }
    };
    let layout = Layout::of(replay.entries);
    let mut found = Vec::new();
    let mut report = |path: &str, problem: Problem, quoted: Option<&[u8]>| {
        let file = format!("{}/{path}", replay.corpus_dir);
        found.push(problem.finding(&file, quoted));
    };
    for path in &layout.unexpected {
        report(path, Problem::UnexpectedEntry, None);
    }
    if layout.cases.is_empty() {
        report(README, Problem::CorpusEmpty, None);
    }
    for (name, case) in &layout.cases {
        match (case.inputs, case.decision) {
            (Some(inputs), Some(recorded)) => {
                if let Some((file, problem, quoted)) = replay_case(&rubric, inputs, recorded) {
                    report(&format!("{name}/{file}"), problem, Some(quoted));
                }
            }
            (None, _) => report(&format!("{name}/{INPUTS}"), Problem::CaseIncomplete, None),
            (Some(_), None) => {
                report(&format!("{name}/{DECISION}"), Problem::CaseIncomplete, None);
            }
        }
    }
    found.sort_by(|a, b| a.file.cmp(&b.file));
    found
}

/// Why a rubric file was refused: it is not UTF-8 TOML of the rubric's shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the rubric does not parse at line {line}")]
pub struct RubricError {
    /// The 1-based line the first error points at (1 when it points nowhere).
    pub line: usize,
}

/// A rubric file (`rubric/v1.toml`), read for a binary that embeds the shipped one.
///
/// # Errors
///
/// [`RubricError`] when it is not UTF-8 TOML of the rubric's shape.
///
/// # Examples
///
/// ```
/// let rubric = assay_app::read_rubric(
///     br#"
/// version = "1"
/// majors_block_threshold = 3
/// max_requeues = 3
/// micro_roles = ["combined-micro"]
/// "#,
/// )?;
/// assert_eq!(rubric.version(), "1");
/// # Ok::<(), assay_app::RubricError>(())
/// ```
pub fn read_rubric(bytes: &[u8]) -> Result<Rubric, RubricError> {
    parse_rubric(bytes).map_err(|line| RubricError { line })
}

/// The rubric, or the 1-based line its parse error points at (1 when it points nowhere).
pub(crate) fn parse_rubric(bytes: &[u8]) -> Result<Rubric, usize> {
    let text =
        str::from_utf8(bytes).map_err(|error| report::line_at(bytes, error.valid_up_to()))?;
    toml::from_str(text).map_err(|error: toml::de::Error| {
        error
            .span()
            .map_or(1, |span| report::line_at(bytes, span.start))
    })
}

/// Why one case does not replay to its recorded bytes: the file at fault (`inputs.json` or
/// `decision.json`), the problem and the bytes to quote; `None` when it replays exactly.
fn replay_case<'a>(
    rubric: &Rubric,
    inputs: &'a [u8],
    recorded: &'a [u8],
) -> Option<(&'static str, Problem, &'a [u8])> {
    let parsed: DecisionInputs = match serde_json::from_slice(inputs) {
        Ok(parsed) => parsed,
        Err(error) => {
            let problem = Problem::InputsInvalid { line: error.line() };
            return Some((INPUTS, problem, inputs));
        }
    };
    let decision = decide(rubric, &parsed);
    let Ok(mut written) = serde_json::to_vec_pretty(&decision) else {
        return Some((DECISION, Problem::Unwritable, recorded));
    };
    written.push(b'\n');
    let differs_at = recorded
        .iter()
        .zip(&written)
        .position(|(recorded, written)| recorded != written)
        .or_else(|| (recorded.len() != written.len()).then(|| recorded.len().min(written.len())))?;
    let line = report::line_at(recorded, differs_at);
    Some((DECISION, Problem::Mismatch { line }, recorded))
}

/// The corpus sorted into cases, with every entry that does not belong.
#[derive(Debug, Default)]
struct Layout<'a> {
    cases: BTreeMap<&'a str, Case<'a>>,
    unexpected: Vec<&'a str>,
    /// Whether the root `README.md` was seen.
    readme: bool,
}

/// One case's two files, as far as they were found.
#[derive(Debug, Default)]
struct Case<'a> {
    /// Whether the case's directory entry was seen.
    listed: bool,
    inputs: Option<&'a [u8]>,
    decision: Option<&'a [u8]>,
}

impl<'a> Layout<'a> {
    /// Sorts `entries` by the corpus allowlist: a root `README.md`, case directories, and in
    /// each case its `inputs.json` and `decision.json` as regular files, each once.
    fn of(entries: &'a [CorpusEntry]) -> Self {
        let mut layout = Self::default();
        for entry in entries {
            let segments: Vec<&str> = entry.path.split('/').collect();
            // Each entry is placed once; a repeat is refused and the first one kept.
            let placed = match (segments.as_slice(), &entry.kind) {
                ([README], CorpusEntryKind::File(_)) => !mem::replace(&mut layout.readme, true),
                ([name], CorpusEntryKind::Directory) if is_name(name) => {
                    let case = layout.cases.entry(name).or_default();
                    !mem::replace(&mut case.listed, true)
                }
                ([name, file], CorpusEntryKind::File(bytes)) if is_name(name) => {
                    let case = layout.cases.entry(name).or_default();
                    let slot = match *file {
                        INPUTS => Some(&mut case.inputs),
                        DECISION => Some(&mut case.decision),
                        _ => None,
                    };
                    match slot {
                        Some(empty @ None) => {
                            *empty = Some(bytes);
                            true
                        }
                        _ => false,
                    }
                }
                _ => false,
            };
            if !placed {
                layout.unexpected.push(&entry.path);
            }
        }
        layout
    }
}

/// Whether `segment` names a directory entry: not empty, `.` or `..`.
fn is_name(segment: &str) -> bool {
    !matches!(segment, "" | "." | "..")
}
