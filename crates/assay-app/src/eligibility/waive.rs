//! Waivers across heads and the events of applied privileged commands (04 §5 lifetime table,
//! R-08, R3-12, R3-13, #59).
//!
//! A waiver lives for the head it was posted on. It is carried to a later head only when the
//! previous completed run's decision waived the same fingerprint and every gate document
//! carrying it there has the same binding digest here, and only through a waive record whose
//! comment still stands and whose actor is eligible on the new head (wi-b5affa). An override is
//! never carried.
//!
//! A document's binding digest is the sha256 of its canonical JSON without the fields that name
//! the run (wi-bc5341): every gate writes its own head's evidence key into `source.run_key`
//! (03 §3), so whole-document digests never match across heads. `source.run_key` is the only
//! such field of `assay.findings/1`: `schema`, `source.kind`, `source.name`, `source.version`
//! (the gate's pin), `source.role`, `conclusion`, `findings` and `stats` describe what the gate
//! found, not which run asked. Canonical means parsed strictly, object keys sorted, written
//! compactly, so a change of key order or spacing does not matter and any change of content
//! does.

use std::collections::{BTreeMap, BTreeSet};

use assay_domain::{Decision, EventPayload, Fingerprint, GateReport, ReasonCode, Waiver};
use serde_json::{Map, Value};

use super::commands::{CommandRecord, CommandStatus};
use super::grammar::Command;
use super::state::GitHubState;
use crate::config::Sha256Digest;
use crate::contract::parse_json_strict;

/// What a waiver of each gate fingerprint of one head is bound to: the binding digests of the
/// gate documents carrying it (R3-13).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WaiverBindings {
    bound: BTreeMap<Fingerprint, BTreeSet<Sha256Digest>>,
}

impl WaiverBindings {
    /// The bindings of `gates`, whose documents are `documents` as `(file, bytes)` pairs.
    ///
    /// A fingerprint is bound to the binding digest of every document named
    /// `<gate>.json`, in any gates job, of every gate carrying it, so a gate run in two jobs
    /// binds both documents. A fingerprint one of whose gates has no document, or a document
    /// that does not parse, is not bound at all, so it is never carried.
    pub(crate) fn of<'d>(
        gates: &[GateReport],
        documents: impl IntoIterator<Item = (&'d str, &'d [u8])>,
    ) -> Self {
        let mut by_file: BTreeMap<&str, Vec<Option<Sha256Digest>>> = BTreeMap::new();
        for (file, bytes) in documents {
            by_file.entry(file).or_default().push(binding_digest(bytes));
        }
        let mut bound: BTreeMap<Fingerprint, BTreeSet<Sha256Digest>> = BTreeMap::new();
        let mut unbound = BTreeSet::new();
        for gate in gates {
            let file = format!("{}.json", gate.name);
            let digests = by_file.get(file.as_str());
            for finding in &gate.findings {
                let Some(digests) = digests else {
                    unbound.insert(finding.fp);
                    continue;
                };
                let set = bound.entry(finding.fp).or_default();
                for digest in digests {
                    match digest {
                        // Each fingerprint's set owns its copy: several share one document.
                        Some(digest) => {
                            set.insert(digest.clone());
                        }
                        None => {
                            unbound.insert(finding.fp);
                        }
                    }
                }
            }
        }
        bound.retain(|fp, _| !unbound.contains(fp));
        Self { bound }
    }

    /// The digests a waiver of `fp` is bound to on this head; `None` when no gate finding
    /// carries it, or one that does has no document.
    #[must_use]
    pub fn digests(&self, fp: Fingerprint) -> Option<&BTreeSet<Sha256Digest>> {
        self.bound.get(&fp)
    }
}

/// The binding digest of a gate document: the sha256 of its canonical JSON with
/// `source.run_key` removed (see the module docs), or `None` when it does not parse strictly.
fn binding_digest(bytes: &[u8]) -> Option<Sha256Digest> {
    let mut document = parse_json_strict(bytes).ok()?;
    if let Some(source) = document.get_mut("source").and_then(Value::as_object_mut) {
        source.remove("run_key");
    }
    let canonical = serde_json::to_vec(&sorted(document)).ok()?;
    Some(Sha256Digest::of(&canonical))
}

/// `value` with every object's keys in sorted order, whatever order the map type keeps.
fn sorted(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map.into_iter().collect();
            entries.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, sorted(value)))
                    .collect::<Map<String, Value>>(),
            )
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
        scalar @ (Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)) => scalar,
    }
}

/// What the previous completed run's decision waived, each fingerprint with its binding there.
///
/// Read by [`prior_waivers`](crate::prior_waivers); empty when there is no such run or its
/// carry-over is reset.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PriorWaivers {
    waived: BTreeMap<Fingerprint, BTreeSet<Sha256Digest>>,
}

impl PriorWaivers {
    /// The fingerprints of `waived` that `bindings` binds, each with its binding.
    pub(crate) fn new(
        waived: impl IntoIterator<Item = Fingerprint>,
        bindings: WaiverBindings,
    ) -> Self {
        let mut bound = bindings.bound;
        let waived = waived
            .into_iter()
            .filter_map(|fp| bound.remove(&fp).map(|digests| (fp, digests)))
            .collect();
        Self { waived }
    }

    /// The fingerprints the previous decision waived and bound, in order.
    pub fn fingerprints(&self) -> impl Iterator<Item = Fingerprint> + '_ {
        self.waived.keys().copied()
    }

    /// The digests `fp`'s waiver was bound to on the previous head, if it was waived there.
    #[must_use]
    pub fn digests(&self, fp: Fingerprint) -> Option<&BTreeSet<Sha256Digest>> {
        self.waived.get(&fp)
    }
}

impl GitHubState {
    /// The waivers of this head for [`DecisionInputs::waivers`](assay_domain::DecisionInputs):
    /// [`GitHubState::waivers`], then each waiver carried from the previous completed run.
    ///
    /// A fingerprint `prior` waived is carried when no record of this head waives it already,
    /// its binding there is not empty and equals its binding here in `bindings`, and a waive
    /// record of it from an earlier head still stands with an actor eligible on this head; the
    /// earliest such record names who waived it.
    #[must_use]
    pub fn waivers_carried(&self, prior: &PriorWaivers, bindings: &WaiverBindings) -> Vec<Waiver> {
        let mut waivers = self.waivers();
        let here: BTreeSet<Fingerprint> = waivers.iter().map(|waiver| waiver.fp).collect();
        let carried: Vec<Waiver> = prior
            .waived
            .iter()
            .filter(|&(fp, digests)| {
                !here.contains(fp) && !digests.is_empty() && bindings.digests(*fp) == Some(digests)
            })
            .filter_map(|(&fp, _)| {
                self.standing_waive(fp).map(|record| Waiver {
                    fp,
                    by: self.login(record.actor),
                })
            })
            .collect();
        waivers.extend(carried);
        waivers
    }

    /// The `OverrideApplied` event of `decision`, when it applied an override: who issued this
    /// head's latest valid override (the one [`GitHubState::override_record`] gives), and the
    /// reason from its record.
    #[must_use]
    pub fn override_applied(&self, decision: &Decision) -> Option<EventPayload> {
        if !decision
            .reasons
            .iter()
            .any(|reason| reason.code == ReasonCode::Override)
        {
            return None;
        }
        let record = self.latest_override()?;
        match &record.command {
            Command::Override { reason, .. } => Some(EventPayload::OverrideApplied {
                who: self.login(record.actor),
                // The event owns its copy of the reason; the record keeps its own.
                reason: reason.clone(),
            }),
            _ => None,
        }
    }

    /// The earliest waive record of `fp` posted on another head whose comment still stands
    /// and whose actor is eligible on this head.
    fn standing_waive(&self, fp: Fingerprint) -> Option<&CommandRecord> {
        self.commands
            .iter()
            .filter(|judged| judged.status == CommandStatus::OtherHead)
            .map(|judged| &judged.record)
            .filter(|record| matches!(record.command, Command::Waive { finding } if finding == fp))
            .filter(|record| self.rule.judge(record.actor).is_ok())
            .min_by_key(|record| (record.created_at, record.comment_id))
    }
}

/// One `WaiverApplied` event per waiver `decision` applied, in its `waived` order.
pub fn waiver_events(decision: &Decision) -> impl Iterator<Item = EventPayload> + '_ {
    decision
        .waived
        .iter()
        .map(|waived| EventPayload::WaiverApplied {
            // Each event owns its copy of who waived; the decision keeps its own.
            who: waived.by.clone(),
            fp: waived.fp,
        })
}
