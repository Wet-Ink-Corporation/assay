//! The coordinator's answer: the schema the CLI is given and the strict reading of what comes
//! back.
//!
//! The CLI checks structured output against [`COORDINATION_SCHEMA`], but the output is still
//! model text, so it is read again here against the findings the coordinator was shown, and
//! refused whole on anything outside the exact shape or any rule (wi-b5affa). The answer holds
//! finding ids only: nothing in it is published.

use std::num::NonZeroU64;

use assay_domain::Severity;
use serde_json::{Map, Value};

use super::merge::may_absorb;
use super::outcome::Merged;
use crate::validation::ValidFinding;

/// The JSON Schema the coordinator's structured output must satisfy (`--json-schema`): groups
/// of finding ids, each with its duplicates, and the ids of noise.
pub const COORDINATION_SCHEMA: &str = r#"{"type":"object","additionalProperties":false,"required":["groups","noise"],"properties":{"groups":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["findings","duplicates"],"properties":{"findings":{"type":"array","minItems":1,"items":{"type":"integer","minimum":0}},"duplicates":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["id","of"],"properties":{"id":{"type":"integer","minimum":0},"of":{"type":"integer","minimum":0}}}}}}},"noise":{"type":"array","items":{"type":"integer","minimum":0}}}}"#;

/// Why an answer was refused, kept so refusal causes can be measured. Never quotes the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CoordinationRefusal {
    /// Not an object of exactly `groups` and `noise`, a group not of exactly `findings` and
    /// `duplicates`, a duplicate not of exactly `id` and `of`, or an id not a whole number.
    Shape,
    /// An id names no finding shown.
    UnknownFinding,
    /// An id is placed twice, or listed twice as noise.
    Repeated,
    /// A finding shown is in no group.
    Missing,
    /// A group lists no finding.
    EmptyGroup,
    /// A group's findings do not share one `class`.
    MixedClasses,
    /// A group holds more than one finding of at least `major`: grouping them would lower the
    /// count of distinct majors (04 §5 rule 7), which only the mechanical merge may do.
    SeveralMajors,
    /// A duplicate's `of` is not among its own group's findings.
    DuplicateOutsideGroup,
    /// A duplicate cites another file or side than the finding it duplicates.
    DuplicateElsewhere,
    /// A duplicate is `major` or above: only the mechanical merge of one fingerprint may absorb
    /// such a finding, which would otherwise be neither published nor verified.
    DuplicateTooSevere,
    /// A duplicate has another `class` than the finding it duplicates.
    DuplicateOtherClass,
    /// A duplicate has another `kind` than the finding it duplicates.
    DuplicateOtherKind,
    /// A duplicate's lines do not overlap those of the finding it duplicates.
    DuplicateApart,
    /// A duplicate is more severe than the finding it duplicates, or is not pre-existing while
    /// that one is: merging it would hide it.
    DuplicateOutranks,
    /// Noise names a `major` or `blocker`.
    NoiseTooSevere,
    /// Noise names a duplicate, which is not kept.
    NoiseMerged,
}

/// A finding shown to the coordinator: its id is its position among those shown, and `index`
/// its position in the findings given to the stage.
#[derive(Debug, Clone, Copy)]
pub(super) struct Shown<'a> {
    pub(super) index: usize,
    pub(super) found: &'a ValidFinding,
}

/// A kept finding of an accepted answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Member {
    /// Its position in the findings given to the stage.
    pub(super) index: usize,
    /// Whether the coordinator marked it as noise.
    pub(super) noise: bool,
}

/// An accepted answer, in positions of the findings given to the stage: every id is mapped
/// through [`Shown::index`] as it is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Plan {
    /// Each group's findings, most important first; the groups in the same order.
    pub(super) groups: Vec<Vec<Member>>,
    /// Each duplicate and the kept finding it joins, in the answer's order.
    pub(super) duplicates: Vec<Merged>,
}

/// Reads `answer` against the findings `shown`, refusing anything outside the exact shape or
/// any rule. Linear in the smaller of the answer and the findings: every accepted id marks one
/// finding, so the answer is refused by the time it names one more id than there are findings.
pub(super) fn read(answer: Value, shown: &[Shown<'_>]) -> Result<Plan, CoordinationRefusal> {
    let mut fields = exact(answer, ["groups", "noise"])?;
    let (Some(Value::Array(groups)), Some(Value::Array(noise))) =
        (fields.remove("groups"), fields.remove("noise"))
    else {
        return Err(CoordinationRefusal::Shape);
    };
    let mut reader = Reader {
        shown,
        member_at: vec![None; shown.len()],
        placed: vec![false; shown.len()],
        plan: Plan {
            groups: Vec::new(),
            duplicates: Vec::new(),
        },
    };
    for group in groups {
        reader.group(group)?;
    }
    if reader.placed.contains(&false) {
        return Err(CoordinationRefusal::Missing);
    }
    for id in &noise {
        reader.noise(id)?;
    }
    Ok(reader.plan)
}

/// The fields of `value`, which must be an object of exactly `keys`.
fn exact<const N: usize>(
    value: Value,
    keys: [&str; N],
) -> Result<Map<String, Value>, CoordinationRefusal> {
    match value {
        Value::Object(fields)
            if fields.len() == N && keys.iter().all(|key| fields.contains_key(*key)) =>
        {
            Ok(fields)
        }
        _ => Err(CoordinationRefusal::Shape),
    }
}

/// Whether `finding` counts for rule 7's distinct majors: at least `major`.
fn is_major(found: &ValidFinding) -> bool {
    found.finding.severity >= Severity::Major
}

/// The lines `found` cites, first and last.
fn span(found: &ValidFinding) -> (NonZeroU64, NonZeroU64) {
    let first = found.finding.line;
    (first, found.finding.end_line.unwrap_or(first).max(first))
}

/// The answer read so far.
struct Reader<'a, 'f> {
    shown: &'a [Shown<'f>],
    /// Where each finding sits as a member, `(group, position)`, by id.
    member_at: Vec<Option<(usize, usize)>>,
    /// Whether each finding is placed, as a member or a duplicate, by id.
    placed: Vec<bool>,
    plan: Plan,
}

impl<'f> Reader<'_, 'f> {
    /// The finding `value` names: a whole number below the number of findings shown.
    fn id(&self, value: &Value) -> Result<(usize, Shown<'f>), CoordinationRefusal> {
        let id = value.as_u64().ok_or(CoordinationRefusal::Shape)?;
        usize::try_from(id)
            .ok()
            .and_then(|id| Some((id, *self.shown.get(id)?)))
            .ok_or(CoordinationRefusal::UnknownFinding)
    }

    /// Places the finding `value` names, which must not be placed yet.
    fn place(&mut self, value: &Value) -> Result<(usize, Shown<'f>), CoordinationRefusal> {
        let (id, shown) = self.id(value)?;
        match self.placed.get_mut(id) {
            Some(placed) if !*placed => {
                *placed = true;
                Ok((id, shown))
            }
            _ => Err(CoordinationRefusal::Repeated),
        }
    }

    /// Reads one group: its findings, which share a class and hold at most one major, then its
    /// duplicates, each of a finding among them.
    fn group(&mut self, group: Value) -> Result<(), CoordinationRefusal> {
        let mut fields = exact(group, ["findings", "duplicates"])?;
        let (Some(Value::Array(members)), Some(Value::Array(duplicates))) =
            (fields.remove("findings"), fields.remove("duplicates"))
        else {
            return Err(CoordinationRefusal::Shape);
        };
        let number = self.plan.groups.len();
        let mut kept = Vec::with_capacity(members.len().min(self.shown.len()));
        let mut class = None;
        let mut majors = 0_usize;
        for member in &members {
            let (id, shown) = self.place(member)?;
            let finding = &shown.found.finding;
            if *class.get_or_insert(finding.class) != finding.class {
                return Err(CoordinationRefusal::MixedClasses);
            }
            if is_major(shown.found) {
                majors = majors.saturating_add(1);
                if majors > 1 {
                    return Err(CoordinationRefusal::SeveralMajors);
                }
            }
            if let Some(at) = self.member_at.get_mut(id) {
                *at = Some((number, kept.len()));
            }
            kept.push(Member {
                index: shown.index,
                noise: false,
            });
        }
        if kept.is_empty() {
            return Err(CoordinationRefusal::EmptyGroup);
        }
        for duplicate in duplicates {
            let merged = self.duplicate(duplicate, number)?;
            self.plan.duplicates.push(merged);
        }
        self.plan.groups.push(kept);
        Ok(())
    }

    /// Reads one duplicate of group `number`: `{id, of}`, `of` a member of that group, the
    /// duplicate below `major`, of the same class and kind, on overlapping lines of the same
    /// file and side, and the merge allowed by [`may_absorb`].
    fn duplicate(
        &mut self,
        duplicate: Value,
        number: usize,
    ) -> Result<Merged, CoordinationRefusal> {
        let fields = exact(duplicate, ["id", "of"])?;
        let (Some(id), Some(of)) = (fields.get("id"), fields.get("of")) else {
            return Err(CoordinationRefusal::Shape);
        };
        let (_, copy) = self.place(id)?;
        let (of, keeper) = self.id(of)?;
        if self
            .member_at
            .get(of)
            .copied()
            .flatten()
            .map(|(group, _)| group)
            != Some(number)
        {
            return Err(CoordinationRefusal::DuplicateOutsideGroup);
        }
        let (found, kept) = (copy.found, keeper.found);
        let refusal =
            if kept.file_at_head != found.file_at_head || kept.finding.side != found.finding.side {
                CoordinationRefusal::DuplicateElsewhere
            } else if is_major(found) {
                CoordinationRefusal::DuplicateTooSevere
            } else if kept.finding.class != found.finding.class {
                CoordinationRefusal::DuplicateOtherClass
            } else if kept.finding.kind != found.finding.kind {
                CoordinationRefusal::DuplicateOtherKind
            } else if !overlap(span(kept), span(found)) {
                CoordinationRefusal::DuplicateApart
            } else if !may_absorb(kept, found) {
                CoordinationRefusal::DuplicateOutranks
            } else {
                return Ok(Merged {
                    index: copy.index,
                    into: keeper.index,
                    fp: found.fingerprint,
                });
            };
        Err(refusal)
    }

    /// Reads one noise id: a member of a group, at most `minor`, listed once.
    fn noise(&mut self, value: &Value) -> Result<(), CoordinationRefusal> {
        let (id, shown) = self.id(value)?;
        let member = self
            .member_at
            .get(id)
            .copied()
            .flatten()
            .and_then(|(group, position)| self.plan.groups.get_mut(group)?.get_mut(position))
            .ok_or(CoordinationRefusal::NoiseMerged)?;
        if shown.found.finding.severity > Severity::Minor {
            return Err(CoordinationRefusal::NoiseTooSevere);
        }
        if member.noise {
            return Err(CoordinationRefusal::Repeated);
        }
        member.noise = true;
        Ok(())
    }
}

/// Whether two line spans share a line.
fn overlap(a: (NonZeroU64, NonZeroU64), b: (NonZeroU64, NonZeroU64)) -> bool {
    a.0 <= b.1 && b.0 <= a.1
}

#[cfg(test)]
mod tests;
