//! The coordinator's answer: the schema the CLI is given and the strict reading of what comes
//! back.
//!
//! The CLI checks structured output against [`COORDINATION_SCHEMA`], but the output is still
//! model text, so it is read again here against the findings the coordinator was shown, and
//! refused whole on anything outside the exact shape or any rule (wi-b5affa). The answer holds
//! finding ids only: nothing in it is published.

use assay_domain::Severity;
use serde_json::{Map, Value};

use super::merge::may_absorb;
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
    /// A duplicate's `of` is not among its own group's findings.
    DuplicateOutsideGroup,
    /// A duplicate cites another file or side than the finding it duplicates.
    DuplicateElsewhere,
    /// A duplicate is more severe than the finding it duplicates, or is not pre-existing while
    /// that one is: merging it would hide it.
    DuplicateOutranks,
    /// Noise names a `major` or `blocker`.
    NoiseTooSevere,
    /// Noise names a duplicate, which is not kept.
    NoiseMerged,
}

/// An accepted answer, in ids of the findings shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Plan {
    /// Each group's findings, most important first; the groups in the same order.
    pub(super) groups: Vec<Vec<usize>>,
    /// Each duplicate and the finding it duplicates, `(id, of)`, in the answer's order.
    pub(super) duplicates: Vec<(usize, usize)>,
    /// Whether each finding shown is noise, by id.
    pub(super) noise: Vec<bool>,
}

/// Reads `answer` against the findings `shown`, whose positions are their ids, refusing
/// anything outside the exact shape or any rule. Linear in the smaller of the answer and the
/// findings: every accepted id marks one finding, so the answer is refused by the time it
/// names one more id than there are findings.
pub(super) fn read(answer: Value, shown: &[&ValidFinding]) -> Result<Plan, CoordinationRefusal> {
    let mut fields = exact(answer, ["groups", "noise"])?;
    let (Some(Value::Array(groups)), Some(Value::Array(noise))) =
        (fields.remove("groups"), fields.remove("noise"))
    else {
        return Err(CoordinationRefusal::Shape);
    };
    let mut reader = Reader {
        shown,
        group_of: vec![None; shown.len()],
        placed: vec![false; shown.len()],
        plan: Plan {
            groups: Vec::new(),
            duplicates: Vec::new(),
            noise: vec![false; shown.len()],
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

/// The answer read so far.
struct Reader<'a, 'f> {
    shown: &'a [&'f ValidFinding],
    /// The group each finding is a member of, by id.
    group_of: Vec<Option<usize>>,
    /// Whether each finding is placed, as a member or a duplicate, by id.
    placed: Vec<bool>,
    plan: Plan,
}

impl<'f> Reader<'_, 'f> {
    /// The id `value` names: a whole number below the number of findings shown.
    fn id(&self, value: &Value) -> Result<(usize, &'f ValidFinding), CoordinationRefusal> {
        let id = value.as_u64().ok_or(CoordinationRefusal::Shape)?;
        usize::try_from(id)
            .ok()
            .and_then(|id| Some((id, *self.shown.get(id)?)))
            .ok_or(CoordinationRefusal::UnknownFinding)
    }

    /// Places the finding `value` names, which must not be placed yet.
    fn place(&mut self, value: &Value) -> Result<(usize, &'f ValidFinding), CoordinationRefusal> {
        let (id, found) = self.id(value)?;
        match self.placed.get_mut(id) {
            Some(placed) if !*placed => {
                *placed = true;
                Ok((id, found))
            }
            _ => Err(CoordinationRefusal::Repeated),
        }
    }

    /// Reads one group: its findings, which share a class, then its duplicates, each of a
    /// finding among them.
    fn group(&mut self, group: Value) -> Result<(), CoordinationRefusal> {
        let mut fields = exact(group, ["findings", "duplicates"])?;
        let (Some(Value::Array(members)), Some(Value::Array(duplicates))) =
            (fields.remove("findings"), fields.remove("duplicates"))
        else {
            return Err(CoordinationRefusal::Shape);
        };
        let number = self.plan.groups.len();
        let mut ids = Vec::with_capacity(members.len().min(self.shown.len()));
        let mut class = None;
        for member in &members {
            let (id, found) = self.place(member)?;
            if *class.get_or_insert(found.finding.class) != found.finding.class {
                return Err(CoordinationRefusal::MixedClasses);
            }
            if let Some(group) = self.group_of.get_mut(id) {
                *group = Some(number);
            }
            ids.push(id);
        }
        if ids.is_empty() {
            return Err(CoordinationRefusal::EmptyGroup);
        }
        for duplicate in duplicates {
            let pair = self.duplicate(duplicate, number)?;
            self.plan.duplicates.push(pair);
        }
        self.plan.groups.push(ids);
        Ok(())
    }

    /// Reads one duplicate of group `number`: `{id, of}`, `of` a member of that group, and the
    /// merge allowed by [`may_absorb`].
    fn duplicate(
        &mut self,
        duplicate: Value,
        number: usize,
    ) -> Result<(usize, usize), CoordinationRefusal> {
        let fields = exact(duplicate, ["id", "of"])?;
        let (Some(id), Some(of)) = (fields.get("id"), fields.get("of")) else {
            return Err(CoordinationRefusal::Shape);
        };
        let (id, found) = self.place(id)?;
        let (of, keeper) = self.id(of)?;
        if self.group_of.get(of).copied().flatten() != Some(number) {
            return Err(CoordinationRefusal::DuplicateOutsideGroup);
        }
        if keeper.file_at_head != found.file_at_head || keeper.finding.side != found.finding.side {
            return Err(CoordinationRefusal::DuplicateElsewhere);
        }
        if !may_absorb(keeper, found) {
            return Err(CoordinationRefusal::DuplicateOutranks);
        }
        Ok((id, of))
    }

    /// Reads one noise id: a member of a group, at most `minor`, listed once.
    fn noise(&mut self, value: &Value) -> Result<(), CoordinationRefusal> {
        let (id, found) = self.id(value)?;
        if self.group_of.get(id).copied().flatten().is_none() {
            return Err(CoordinationRefusal::NoiseMerged);
        }
        if found.finding.severity > Severity::Minor {
            return Err(CoordinationRefusal::NoiseTooSevere);
        }
        match self.plan.noise.get_mut(id) {
            Some(noise) if !*noise => {
                *noise = true;
                Ok(())
            }
            _ => Err(CoordinationRefusal::Repeated),
        }
    }
}

#[cfg(test)]
mod tests;
