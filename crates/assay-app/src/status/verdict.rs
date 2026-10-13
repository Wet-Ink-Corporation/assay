//! The App's checks on the head: the verdict check, the decision its evidence pointer names,
//! and the per-gate checks.

use std::collections::BTreeMap;

use assay_domain::{Decision, DecisionInputs, RunUid};

use super::ports::{AppId, CheckView, ObservedConclusion, PullView, StatusForge};
use super::{
    BundleProblem, MAX_CHECK_RUNS, StatusError, StatusPorts, StatusRequest, StatusStep,
    StatusUnreadable, page,
};
use crate::config::{GateName, Sha256Digest};
use crate::publish::read_text;
use crate::publish::{BUNDLE_FILE, CheckName, CheckStatus, EvidencePointer, MAX_BUNDLE_BYTES};
use crate::replay::{self, BundleSource, StoredBundle};

/// What a gate check's name starts with (`assay / gate: <plugin>`, 04 §6).
const GATE_PREFIX: &str = "assay / gate: ";

/// What the App's checks on the head say.
pub(super) struct Observed {
    /// The verdict check, if the App has one.
    pub(super) check: Option<Check>,
    /// The decision the check carries, if it names a persisted one.
    pub(super) decided: Option<Decided>,
    /// The per-gate checks, by plugin.
    pub(super) gates: BTreeMap<GateName, Check>,
}

/// One App check as the status shows it.
pub(super) struct Check {
    pub(super) status: CheckStatus,
    pub(super) conclusion: Option<ObservedConclusion>,
    pub(super) url: String,
}

/// A decision read from its authenticated bundle.
pub(super) struct Decided {
    pub(super) decision: Decision,
    pub(super) inputs: DecisionInputs,
}

/// What the verdict check's `external_id` says.
enum Stamp<'a> {
    /// `open`'s `pending:<run_id>:<seq>`: a run is in progress and nothing is decided yet.
    Open,
    /// A decision key: `decide` wrote the check.
    Decided(&'a str),
}

/// Reads the head's check runs and, when the verdict check names a persisted decision, its
/// bundle.
pub(super) fn observe<F: StatusForge, B: BundleSource>(
    request: &StatusRequest<'_>,
    ports: &StatusPorts<'_, F, B>,
    pull: &PullView,
) -> Result<Observed, StatusError<F::Error, B::Error>> {
    let runs = ports
        .forge
        .check_runs(&pull.head_sha)
        .map_err(|source| StatusError::Forge {
            step: StatusStep::CheckRuns,
            source,
        })?;
    if runs.len() > MAX_CHECK_RUNS {
        return Err(StatusError::TooManyChecks { count: runs.len() });
    }
    let repository = request.repository.name.as_str();
    let name = CheckName::for_base(&pull.base_ref, &request.repository.default_branch);
    let verdicts: Vec<&CheckView> = app_checks(&runs, request.app)
        .filter(|run| run.name == name.as_str())
        .collect();
    let (check, decided) = match verdicts.as_slice() {
        [] => (None, None),
        [verdict] => {
            let check = check(verdict, repository).map_err(StatusError::Unreadable)?;
            let decided = match stamp(verdict).map_err(StatusError::Unreadable)? {
                Stamp::Open => None,
                Stamp::Decided(key) => decision(request, ports.bundles, verdict, key)?,
            };
            (Some(check), decided)
        }
        many => return Err(StatusError::DuplicateVerdict { count: many.len() }),
    };
    Ok(Observed {
        check,
        decided,
        gates: gates(&runs, request.app, repository)?,
    })
}

/// The check runs `app` created.
fn app_checks(runs: &[CheckView], app: AppId) -> impl Iterator<Item = &CheckView> {
    runs.iter().filter(move |run| run.app == app)
}

/// `run` as the status shows it, when its status and conclusion agree and its page reads.
fn check(run: &CheckView, repository: &str) -> Result<Check, StatusUnreadable> {
    let completed = run.status == CheckStatus::Completed;
    if completed != run.conclusion.is_some() {
        return Err(StatusUnreadable::CheckState);
    }
    Ok(Check {
        status: run.status,
        conclusion: run.conclusion,
        url: page(&run.url, repository)?,
    })
}

/// The App's `assay / gate: <plugin>` checks, by plugin, each plugin once.
fn gates<E, B>(
    runs: &[CheckView],
    app: AppId,
    repository: &str,
) -> Result<BTreeMap<GateName, Check>, StatusError<E, B>> {
    let mut gates = BTreeMap::new();
    for run in app_checks(runs, app) {
        let Some(plugin) = run.name.strip_prefix(GATE_PREFIX) else {
            continue;
        };
        // The validated name owns its copy of the plugin name.
        let plugin = GateName::new(plugin.to_owned())
            .map_err(|_not_a_plugin| StatusError::Unreadable(StatusUnreadable::GateName))?;
        let check = check(run, repository).map_err(StatusError::Unreadable)?;
        if gates.contains_key(&plugin) {
            return Err(StatusError::DuplicateGate { plugin });
        }
        gates.insert(plugin, check);
    }
    Ok(gates)
}

/// What the verdict check's `external_id` says, or that it says neither thing assay writes.
fn stamp(run: &CheckView) -> Result<Stamp<'_>, StatusUnreadable> {
    let id = run
        .external_id
        .as_deref()
        .ok_or(StatusUnreadable::ExternalId)?;
    if let Some(stamp) = id.strip_prefix("pending:") {
        let digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
        return match stamp.split_once(':') {
            Some((run_id, seq)) if digits(run_id) && digits(seq) => Ok(Stamp::Open),
            _ => Err(StatusUnreadable::ExternalId),
        };
    }
    match id.parse::<Sha256Digest>() {
        Ok(_) => Ok(Stamp::Decided(id)),
        Err(_) => Err(StatusUnreadable::ExternalId),
    }
}

/// The decision a decided verdict check names: none when its run persisted no bundle, else the
/// bundle's, once authenticated against the pointer and the check's key.
fn decision<E, B: BundleSource>(
    request: &StatusRequest<'_>,
    bundles: &B,
    run: &CheckView,
    key: &str,
) -> Result<Option<Decided>, StatusError<E, B::Error>> {
    let text = run
        .output_text
        .as_deref()
        .and_then(read_text)
        .filter(|text| text.decision_key == key)
        .ok_or(StatusError::Unreadable(StatusUnreadable::CheckText))?;
    let Some(pointer) = text.pointer else {
        return Ok(None);
    };
    let decided = authenticated(request, bundles, &pointer, key).map_err(StatusError::Bundle)?;
    Ok(Some(decided))
}

/// The bundle `pointer` names, read under its cap, hashing to the pointer, consistent with it
/// and itself, decided under `request.rubric` and carrying the decision keyed `key`.
fn authenticated<B: BundleSource>(
    request: &StatusRequest<'_>,
    bundles: &B,
    pointer: &EvidencePointer,
    key: &str,
) -> Result<Decided, BundleProblem<B::Error>> {
    if pointer.run_attempt.get() != 1 {
        return Err(BundleProblem::NotFirstAttempt);
    }
    let run = RunUid {
        repo_id: request.repository.id,
        run_id: pointer.run_id,
        run_attempt: pointer.run_attempt,
    };
    let bytes = match bundles
        .fetch(run, BUNDLE_FILE, MAX_BUNDLE_BYTES)
        .map_err(BundleProblem::Fetch)?
    {
        StoredBundle::Found(bytes) if bytes.len() > MAX_BUNDLE_BYTES => {
            return Err(BundleProblem::TooLarge);
        }
        StoredBundle::Found(bytes) => bytes,
        StoredBundle::Missing => return Err(BundleProblem::Missing),
        StoredBundle::Expired => return Err(BundleProblem::Expired),
        StoredBundle::TooLarge => return Err(BundleProblem::TooLarge),
    };
    if Sha256Digest::of(&bytes) != pointer.sha256 {
        return Err(BundleProblem::Digest);
    }
    let bundle = replay::read(&bytes).map_err(BundleProblem::Shape)?;
    drop(bytes);
    if replay::inconsistency(&bundle, run).is_some() {
        return Err(BundleProblem::Inconsistent);
    }
    if bundle.rubric_version != request.rubric.version() {
        return Err(BundleProblem::RubricVersion);
    }
    let decision: Decision = serde_json::from_value(bundle.decision)
        .map_err(|_not_a_decision| BundleProblem::Decision)?;
    if decision.key != key {
        return Err(BundleProblem::Decision);
    }
    Ok(Decided {
        decision,
        inputs: bundle.inputs,
    })
}
