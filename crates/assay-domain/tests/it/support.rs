//! Access to the schemas, golden fixtures and the in-tree specification.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::io;
use std::path::PathBuf;

use assay_domain::{
    Authorship, DecisionInputs, GateConclusion, GateFinding, GateOutcome, GateReport, InfraPolicy,
    LlmPolicy, LlmStage, OwnerApproval, Policy, PrClass, PullRequest, Readiness, Reason,
    ReasonCode, ReviewedFinding, Rubric, Severity, StepAttribution, Tier, VerifierVerdict, WeighIn,
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

/// Inputs that pass every rule: a `lite` PR into a protected `main`, one required gate that
/// passed, a completed LLM stage, no findings and no commands.
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
        degraded: Vec::new(),
        waivers: Vec::new(),
        override_record: None,
    }
}

/// A required gate that ran and concluded `conclusion` with `findings`, whose adapter did not
/// attest its steps (what any adapter gives until the findings contract carries step results).
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

/// A gate finding of `rule` at `severity`.
pub(crate) fn gate_finding(fp: &str, rule: &str, severity: Severity) -> GateFinding {
    GateFinding {
        fp: fp.to_owned(),
        rule: Some(rule.to_owned()),
        severity,
    }
}

/// A finding from the `bugs` role in its own group, confirmed by the verifier and introduced by
/// the diff.
pub(crate) fn confirmed(fp: &str, severity: Severity) -> ReviewedFinding {
    ReviewedFinding {
        fp: fp.to_owned(),
        group_id: format!("group-{fp}"),
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
