//! The decision's findings, one per fingerprint, with what the App's inline comments published
//! about each and whether `/assay waive` may clear it.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::num::NonZeroU64;

use assay_domain::{
    Class, DecisionInputs, Disposition, Fingerprint, Severity, waivable_fingerprints,
};

use super::ports::{PullView, StatusForge};
use super::verdict::Decided;
use super::{Detail, MAX_FINDINGS, StatusError, StatusRequest, StatusStep, page};
use crate::publish::{MAX_PRIOR_COMMENTS, clean, read_published};

/// The longest published body `--full` shows, in characters (the schema's cap); a longer one is
/// left out rather than cut.
const MAX_BODY_CHARS: usize = 16_384;

/// One finding as the status lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Finding {
    pub(super) fp: Fingerprint,
    pub(super) severity: Severity,
    pub(super) published: Option<Published>,
    /// What the per-gate check published for the first gate finding carrying the fingerprint:
    /// `<plugin>: <rule>`, cleaned as the check's summary cleans it.
    pub(super) gate_title: Option<String>,
    pub(super) disposition: Option<Disposition>,
    pub(super) waivable: bool,
}

/// What the App's inline comment published about a finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Published {
    pub(super) class: Option<Class>,
    pub(super) title: Option<String>,
    pub(super) file: String,
    pub(super) line: Option<NonZeroU64>,
    pub(super) url: String,
    pub(super) body: Option<String>,
}

/// The decision's gate findings, then its reviewer findings, one entry per fingerprint at the
/// highest severity carrying it; refused past [`MAX_FINDINGS`] before any comment is read.
pub(super) fn findings<F: StatusForge, B>(
    request: &StatusRequest<'_>,
    forge: &F,
    pull: &PullView,
    decided: &Decided,
) -> Result<Vec<Finding>, StatusError<F::Error, B>> {
    let inputs = &decided.inputs;
    let count = inputs
        .gates
        .iter()
        .map(|gate| gate.findings.len())
        .fold(inputs.findings.len(), usize::saturating_add);
    if count > MAX_FINDINGS {
        return Err(StatusError::TooManyFindings { count });
    }
    let entries = carried(inputs, count);
    let mut published = comments(request, forge, pull)?;
    let waivable = waivable_fingerprints(request.rubric, &inputs.gates);
    let last_disposition: BTreeMap<Fingerprint, Disposition> = decided
        .decision
        .dispositions
        .iter()
        .map(|disposition| (disposition.fp, disposition.code))
        .collect();
    Ok(entries
        .into_iter()
        .map(|carrier| Finding {
            fp: carrier.fp,
            severity: carrier.severity,
            published: published.remove(&carrier.fp),
            gate_title: carrier.gate.map(|(gate, rule)| match rule {
                Some(rule) => clean(&[gate, ": ", rule].concat()),
                None => clean(gate),
            }),
            disposition: last_disposition.get(&carrier.fp).copied(),
            waivable: waivable.contains(&carrier.fp),
        })
        .collect())
}

/// One finding the decision carries: its fingerprint, severity and, for a gate finding, its
/// gate and rule.
struct Carried<'a> {
    fp: Fingerprint,
    severity: Severity,
    gate: Option<(&'a str, Option<&'a str>)>,
}

/// The `count` findings `inputs` carries, gate findings first, merged to one per fingerprint in
/// first-seen order at the highest severity, keeping the first gate and rule seen.
fn carried(inputs: &DecisionInputs, count: usize) -> Vec<Carried<'_>> {
    let all = inputs
        .gates
        .iter()
        .flat_map(|gate| {
            gate.findings.iter().map(|finding| Carried {
                fp: finding.fp,
                severity: finding.severity,
                gate: Some((gate.name.as_str(), finding.rule.as_deref())),
            })
        })
        .chain(inputs.findings.iter().map(|finding| Carried {
            fp: finding.fp,
            severity: finding.severity,
            gate: None,
        }));
    let mut entries: Vec<Carried<'_>> = Vec::with_capacity(count);
    let mut index: BTreeMap<Fingerprint, usize> = BTreeMap::new();
    for carrier in all {
        match index.entry(carrier.fp) {
            Entry::Vacant(vacant) => {
                vacant.insert(entries.len());
                entries.push(carrier);
            }
            Entry::Occupied(occupied) => {
                if let Some(kept) = entries.get_mut(*occupied.get()) {
                    kept.severity = kept.severity.max(carrier.severity);
                    kept.gate = kept.gate.or(carrier.gate);
                }
            }
        }
    }
    entries
}

/// The App's inline finding comments by fingerprint, the first for each; refused past
/// [`MAX_PRIOR_COMMENTS`].
fn comments<F: StatusForge, B>(
    request: &StatusRequest<'_>,
    forge: &F,
    pull: &PullView,
) -> Result<BTreeMap<Fingerprint, Published>, StatusError<F::Error, B>> {
    let views = forge
        .review_comments(pull.number, request.app)
        .map_err(|source| StatusError::Forge {
            step: StatusStep::ReviewComments,
            source,
        })?;
    if views.len() > MAX_PRIOR_COMMENTS {
        return Err(StatusError::TooManyComments { count: views.len() });
    }
    let repository = request.repository.name.as_str();
    let mut published = BTreeMap::new();
    for view in views.iter().filter(|view| view.app == Some(request.app)) {
        let Some(comment) = read_published(&view.body) else {
            continue;
        };
        if published.contains_key(&comment.fp) {
            continue;
        }
        let url = page(&view.url, repository).map_err(StatusError::Unreadable)?;
        let body = match request.detail {
            Detail::Summary => None,
            Detail::Full => comment
                .rest
                .filter(|rest| rest.chars().count() <= MAX_BODY_CHARS)
                .map(str::to_owned),
        };
        published.insert(
            comment.fp,
            Published {
                class: comment.class,
                title: comment.title.map(str::to_owned),
                file: clean(&view.path),
                line: view.line,
                url,
                body,
            },
        );
    }
    Ok(published)
}
