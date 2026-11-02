//! Access to the schemas, golden fixtures and the in-tree specification.
//!
//! Twin of `crates/assay-app/tests/it/support.rs`, which reads the same fixtures for the schema
//! checks; the domain cannot depend on the app, so the fixture walk is written in both.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::io;
use std::path::PathBuf;

use assay_domain::{
    Authorship, DecisionInputs, Fingerprint, GateConclusion, GateFinding, GateOutcome, GateReport,
    InfraPolicy, LlmPolicy, LlmStage, OwnerApproval, Policy, PrClass, PullRequest, Readiness,
    Reason, ReasonCode, RequiredRole, ReviewedFinding, Rubric, Severity, StepAttribution,
    SupportOutcome, Tier, VerifierVerdict, WeighIn,
};
use assay_domain::{
    BaselineChange, ChangeSet, FileChange, Glob, GlobError, Lines, PackageDiff, RepoGlobs,
};
use serde::Serialize;
use serde_json::Value;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The parsed schema `schemas/<stem>.schema.json`.
pub(crate) fn schema(stem: &str) -> Result<Value, Box<dyn Error>> {
    let bytes = fs::read(repo_root().join(format!("schemas/{stem}.schema.json")))?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// The string values of the JSON array at `pointer` in `schema`.
pub(crate) fn enum_at(schema: &Value, pointer: &str) -> Result<BTreeSet<String>, String> {
    Ok(schema
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("no enum at {pointer}"))?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect())
}

/// The wire form of each value, which must serialise to a JSON string.
pub(crate) fn wire_codes<T: Serialize>(
    values: impl IntoIterator<Item = T>,
) -> Result<BTreeSet<String>, Box<dyn Error>> {
    values
        .into_iter()
        .map(|value| match serde_json::to_value(value)? {
            Value::String(code) => Ok(code),
            other => Err(format!("not a string code: {other}").into()),
        })
        .collect()
}

/// The first line of `docs/architecture/03-contracts.md` that starts with `prefix`.
pub(crate) fn spec_line(prefix: &str) -> io::Result<String> {
    let spec = fs::read_to_string(repo_root().join("docs/architecture/03-contracts.md"))?;
    spec.lines()
        .find(|line| line.starts_with(prefix))
        .map(str::to_owned)
        .ok_or_else(|| io::Error::other(format!("03-contracts.md has no line starting {prefix:?}")))
}

/// The names quoted in backticks in `text`, without their `{fields}`, with every grouped
/// `StemA|B` expanded to `StemA` and `StemB` (the stem is the first name less its last
/// capitalised word).
pub(crate) fn spec_names(text: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for quoted in text.split('`').skip(1).step_by(2) {
        let bare = quoted.split_once('{').map_or(quoted, |(name, _)| name);
        let mut alternatives = bare.split('|');
        let Some(first) = alternatives.next() else {
            continue;
        };
        let stem = first
            .rfind(|c: char| c.is_ascii_uppercase())
            .and_then(|at| first.get(..at))
            .unwrap_or_default();
        names.insert(first.to_owned());
        for rest in alternatives {
            names.insert(format!("{stem}{rest}"));
        }
    }
    names
}

/// Every valid golden fixture of `contract` (`findings`, `event`, ...), by file stem.
pub(crate) fn valid_fixtures(contract: &str) -> io::Result<BTreeMap<String, Vec<u8>>> {
    fixtures(contract, "valid")
}

/// Every invalid golden fixture of `contract`, by file stem.
pub(crate) fn invalid_fixtures(contract: &str) -> io::Result<BTreeMap<String, Vec<u8>>> {
    fixtures(contract, "invalid")
}

/// Every golden fixture of `contract` that its schema accepts and the domain refuses, by file
/// stem (`schemas/README.md`).
pub(crate) fn domain_invalid_fixtures(contract: &str) -> io::Result<BTreeMap<String, Vec<u8>>> {
    fixtures(contract, "domain-invalid")
}

/// The contracts under `schemas/fixtures/` that hold a `domain-invalid` directory.
pub(crate) fn contracts_with_domain_invalid_fixtures() -> io::Result<BTreeSet<String>> {
    let mut found = BTreeSet::new();
    for entry in fs::read_dir(repo_root().join("schemas/fixtures"))? {
        let path = entry?.path();
        if path.join("domain-invalid").try_exists()? {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| io::Error::other(format!("bad contract dir {}", path.display())))?;
            found.insert(name.to_owned());
        }
    }
    Ok(found)
}

/// Inputs that pass every rule: a `lite` PR into a protected `main`, one required gate that
/// passed, a completed LLM stage in which every specialist role, the coordinator and the
/// verifier completed (so a test may change the tier or classes), no findings and no commands.
pub(crate) fn clean_inputs() -> DecisionInputs {
    DecisionInputs {
        decision_key: "dec-42-1".to_owned(),
        evidence_key: "run-42-1".to_owned(),
        policy: Policy {
            protected_bases: vec!["main".to_owned()],
            llm_blockers: LlmPolicy::Advisory,
            llm_majors: LlmPolicy::Advisory,
            on_infra_failure: InfraPolicy::Warn,
            block_on_infra_for: vec![PrClass::TrustPath, PrClass::ExecSurface],
        },
        pr: PullRequest {
            base_ref: "main".to_owned(),
            open_prs_on_head: 1,
            tier: Tier::Lite,
            classes: Vec::new(),
            owner_approval: OwnerApproval::NotApproved,
            authorship: Authorship::Other,
            readiness: Readiness::Ready,
            requeues: 0,
        },
        gates: vec![gate("temper", GateConclusion::Pass, Vec::new())],
        weigh_in: WeighIn::NotRequired,
        llm: LlmStage::Completed,
        findings: Vec::new(),
        dispositions: Vec::new(),
        completed_roles: RequiredRole::ALL.into_iter().collect(),
        coordinator: SupportOutcome::Completed,
        verifier: SupportOutcome::Completed,
        degraded: Vec::new(),
        waivers: Vec::new(),
        override_record: None,
    }
}

/// Clean inputs made a `full` PR of class trust path that the owner approved, with every
/// required role completed.
pub(crate) fn full_trust_path() -> DecisionInputs {
    let mut inputs = clean_inputs();
    inputs.pr.tier = Tier::Full;
    inputs.pr.classes = vec![PrClass::TrustPath];
    inputs.pr.owner_approval = OwnerApproval::Approved;
    inputs
}

/// A required gate that ran and concluded `conclusion` with `findings`, whose adapter did not
/// attest its steps (what every adapter but temper gives, 03 §2.1).
pub(crate) fn gate(
    name: &str,
    conclusion: GateConclusion,
    findings: Vec<GateFinding>,
) -> GateReport {
    GateReport {
        name: name.to_owned(),
        required: true,
        outcome: GateOutcome::Ran {
            conclusion,
            steps: StepAttribution::Unknown,
        },
        findings,
    }
}

/// A required gate that failed with `findings` and whose adapter attested that no step failed
/// without a finding, so waivers may clear it.
pub(crate) fn attested_gate(name: &str, findings: Vec<GateFinding>) -> GateReport {
    GateReport {
        name: name.to_owned(),
        required: true,
        outcome: GateOutcome::Ran {
            conclusion: GateConclusion::Fail,
            steps: StepAttribution::Known(Vec::new()),
        },
        findings,
    }
}

/// The fingerprint written `text`, 16 lowercase hex digits.
pub(crate) fn fp(text: &str) -> Fingerprint {
    text.parse()
        .expect("test fingerprints are written as 16 lowercase hex digits")
}

/// A gate finding of `rule` at `severity`.
pub(crate) fn gate_finding(fp_text: &str, rule: &str, severity: Severity) -> GateFinding {
    GateFinding {
        fp: fp(fp_text),
        rule: Some(rule.to_owned()),
        severity,
    }
}

/// A finding from the `bugs` role in its own group, confirmed by the verifier and introduced by
/// the diff.
pub(crate) fn confirmed(fp_text: &str, severity: Severity) -> ReviewedFinding {
    ReviewedFinding {
        fp: fp(fp_text),
        group_id: format!("group-{fp_text}"),
        role: "bugs".to_owned(),
        severity,
        pre_existing: false,
        verdict: Some(VerifierVerdict::Confirmed),
    }
}

/// A reason without a fingerprint.
pub(crate) fn reason(code: ReasonCode, detail: &str) -> Reason {
    Reason {
        code,
        detail: detail.to_owned(),
        fp: None,
    }
}

/// One case of `corpus/golden/`: the bytes of its inputs and of its recorded decision.
pub(crate) struct GoldenCase {
    pub(crate) inputs: Vec<u8>,
    pub(crate) decision: Vec<u8>,
}

/// Every case directory of `corpus/golden/`, by name.
pub(crate) fn golden_cases() -> io::Result<BTreeMap<String, GoldenCase>> {
    let mut cases = BTreeMap::new();
    for entry in fs::read_dir(repo_root().join("corpus/golden"))? {
        let dir = entry?.path();
        if !dir.is_dir() {
            continue;
        }
        let name = dir
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::other(format!("bad case name {}", dir.display())))?
            .to_owned();
        let case = GoldenCase {
            inputs: fs::read(dir.join("inputs.json"))?,
            decision: fs::read(dir.join("decision.json"))?,
        };
        cases.insert(name, case);
    }
    Ok(cases)
}

/// `rubric/v1.toml`, parsed.
pub(crate) fn rubric_v1() -> Result<Rubric, Box<dyn Error>> {
    let text = fs::read_to_string(repo_root().join("rubric/v1.toml"))?;
    Ok(toml::from_str(&text)?)
}

fn fixtures(contract: &str, validity: &str) -> io::Result<BTreeMap<String, Vec<u8>>> {
    let dir = repo_root()
        .join("schemas/fixtures")
        .join(contract)
        .join(validity);
    let mut found = BTreeMap::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            let stem = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or_else(|| io::Error::other(format!("bad fixture name {}", path.display())))?
                .to_owned();
            found.insert(stem, fs::read(&path)?);
        }
    }
    Ok(found)
}

/// A modified file with numstat `added` and `removed`.
pub(crate) fn file(path: &str, added: u64, removed: u64) -> FileChange {
    FileChange::new(path, Lines::Counted { added, removed })
}

/// A change set of `files` with no manifest trust keys, no `include_*!` targets, no package
/// changes and no temper baseline change.
pub(crate) fn changes(files: Vec<FileChange>) -> ChangeSet {
    ChangeSet {
        files,
        manifests: Vec::new(),
        include_targets: Vec::new(),
        exec_roots: Vec::new(),
        packages: PackageDiff::Computed(Vec::new()),
        temper_baseline: BaselineChange::Other,
    }
}

/// Repo glob additions, as `.assay/globs.toml` would give them.
pub(crate) fn globs(
    risk: &[&str],
    trust: &[&str],
    exclude: &[&str],
    docs: &[&str],
) -> Result<RepoGlobs, GlobError> {
    let parse = |patterns: &[&str]| -> Result<Vec<Glob>, GlobError> {
        patterns.iter().map(|p| p.parse()).collect()
    };
    Ok(RepoGlobs {
        risk: parse(risk)?,
        trust: parse(trust)?,
        exclude: parse(exclude)?,
        docs: parse(docs)?,
    })
}
