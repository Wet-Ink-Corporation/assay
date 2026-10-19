//! CP1, the local pre-push checkpoint (`docs/architecture/04-pipeline.md` §9): the open
//! Weigh-In items of the current branch, read from `weighin open --repo --json`, sorted into
//! blocking and pending triage by [`assay_domain::standing`].
//!
//! The output is an unversioned dump of weighin's internal item, so it is untrusted
//! (wi-b5affa): it is read through a byte cap and an item cap, a repeated key is refused, and
//! every field assay reads must be present with a value weighin writes, or the whole output is
//! refused. Fields assay does not read are ignored. Every item is checked, whatever its branch.

use std::collections::BTreeSet;

use assay_domain::{
    Authority, BlockingCause, BlockingCauses, Blocks, Door, ParseWeighInIdError, Standing, Triage,
    WeighInId, standing,
};
use serde_json::{Map, Value};

use crate::contract::{MAX_DOCUMENT_BYTES, parse_json_strict};
use crate::finding_text::plain_name;

/// The most bytes of `weighin open` output read: the contract document cap.
pub const MAX_OPEN_BYTES: usize = MAX_DOCUMENT_BYTES;

/// The most items read. `weighin open` lists only its `--top` items and says nothing of the
/// rest, so the caller asks for one more than this and a longer list is refused as possibly
/// truncated.
pub const MAX_OPEN_ITEMS: usize = 4096;

/// The statuses `weighin_core::Status::is_open` covers; `open` lists no other (04 §9).
const OPEN_STATUSES: [&str; 5] = ["raised", "triaged", "grounded", "presented", "deferred"];

/// One blocking item and why it blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockingItem {
    id: WeighInId,
    causes: BlockingCauses,
}

impl BlockingItem {
    /// The item.
    #[must_use]
    pub const fn id(&self) -> &WeighInId {
        &self.id
    }

    /// Why it blocks: at least one cause.
    #[must_use]
    pub const fn causes(&self) -> BlockingCauses {
        self.causes
    }
}

/// The open items of one branch that CP1 reports: blocking ones block the push, pending ones
/// warn. Items that are triaged and clear are not kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    branch: String,
    blocking: Vec<BlockingItem>,
    pending: Vec<WeighInId>,
}

impl Checkpoint {
    /// The blocking items, in weighin's order.
    #[must_use]
    pub fn blocking(&self) -> &[BlockingItem] {
        &self.blocking
    }

    /// The items still awaiting triage, in weighin's order.
    #[must_use]
    pub fn pending(&self) -> &[WeighInId] {
        &self.pending
    }

    /// Whether an item blocks the push.
    #[must_use]
    pub const fn blocks_push(&self) -> bool {
        !self.blocking.is_empty()
    }

    /// The terminal report: one summary line, a line per item, and what the push needs.
    #[must_use]
    pub fn render(&self) -> String {
        let branch = plain_name(&self.branch);
        if self.blocking.is_empty() && self.pending.is_empty() {
            return format!("weigh-in on {branch}: nothing open\n");
        }
        let mut text = format!(
            "weigh-in on {branch}: {} blocking, {} pending triage\n",
            self.blocking.len(),
            self.pending.len()
        );
        for item in &self.blocking {
            text.extend(["  blocking ", item.id.as_str(), ": "]);
            for (n, cause) in item.causes.iter().enumerate() {
                if n > 0 {
                    text.push_str(", ");
                }
                write_cause(&mut text, cause);
            }
            text.push('\n');
        }
        for id in &self.pending {
            text.extend(["  pending triage ", id.as_str(), "\n"]);
        }
        text.push_str(if self.blocks_push() {
            "  push blocked: settle the blocking items with /weigh-in (git push --no-verify skips \
             this check)\n"
        } else {
            "  warning: triage the pending items with /weigh-in before the push\n"
        });
        text
    }
}

/// Appends how `cause` reads in the report to `text`.
fn write_cause(text: &mut String, cause: BlockingCause) {
    match cause {
        BlockingCause::Door(door) => text.extend(["door ", door.code()]),
        BlockingCause::Authority => text.push_str("authority"),
        BlockingCause::Blocks => text.push_str("blocks"),
    }
}

/// Why `weighin open` output was refused. No value from the output is echoed except a
/// validated id ([`OpenItemsError::Duplicate`]).
#[derive(Debug, thiserror::Error)]
pub enum OpenItemsError {
    /// The output is over [`MAX_OPEN_BYTES`]; it was not parsed.
    #[error("weighin open printed {len} bytes, over the {MAX_OPEN_BYTES}-byte cap")]
    TooLarge {
        /// Its length in bytes.
        len: usize,
    },
    /// The output is not JSON, or an object repeats a key.
    #[error("weighin open did not print one JSON document without repeated keys")]
    Json(#[source] serde_json::Error),
    /// The output is not a list.
    #[error("weighin open did not print a list of items")]
    NotAList,
    /// The list holds more than [`MAX_OPEN_ITEMS`] items, so it may be truncated.
    #[error("weighin open listed more than {MAX_OPEN_ITEMS} items, so the list may be cut short")]
    TooMany,
    /// An item is not an object.
    #[error("item {index} of weighin open is not an object")]
    NotAnItem {
        /// Its position in the list, from 0.
        index: usize,
    },
    /// A field assay reads is missing or holds a value weighin does not write.
    #[error("item {index} of weighin open: {field} is missing or not a value weighin writes")]
    Field {
        /// Its item's position in the list, from 0.
        index: usize,
        /// The field.
        field: &'static str,
    },
    /// An item's id is not a Weigh-In id.
    #[error("item {index} of weighin open has an id weighin does not write")]
    Id {
        /// Its item's position in the list, from 0.
        index: usize,
        /// Why the id was refused.
        #[source]
        source: ParseWeighInIdError,
    },
    /// An id is listed twice.
    #[error("item {index} of weighin open repeats the id {id}")]
    Duplicate {
        /// The second item's position in the list, from 0.
        index: usize,
        /// The id.
        id: WeighInId,
    },
}

/// The items of `weighin open --repo --json` output whose `branch` is `branch`, sorted for
/// CP1: triaged ones by [`standing`], and any whose triaged door or authority is still null
/// as pending triage.
///
/// # Errors
///
/// [`OpenItemsError`] when the output is over [`MAX_OPEN_BYTES`], is not a JSON list without
/// repeated keys, lists more than [`MAX_OPEN_ITEMS`] items, or holds an item, on any branch,
/// that is not an object, lacks a field assay reads (`id`, `status`, `branch`, `door`,
/// `authority`, `blocks`) or holds a value weighin does not write there (a status that is not
/// open included), or repeats an id.
pub fn checkpoint(open_json: &[u8], branch: &str) -> Result<Checkpoint, OpenItemsError> {
    if open_json.len() > MAX_OPEN_BYTES {
        return Err(OpenItemsError::TooLarge {
            len: open_json.len(),
        });
    }
    let Value::Array(items) = parse_json_strict(open_json).map_err(OpenItemsError::Json)? else {
        return Err(OpenItemsError::NotAList);
    };
    if items.len() > MAX_OPEN_ITEMS {
        return Err(OpenItemsError::TooMany);
    }
    let mut seen = BTreeSet::new();
    let mut checkpoint = Checkpoint {
        // The checkpoint keeps the branch it reports on.
        branch: branch.to_owned(),
        blocking: Vec::new(),
        pending: Vec::new(),
    };
    for (index, item) in items.iter().enumerate() {
        let item = item
            .as_object()
            .ok_or(OpenItemsError::NotAnItem { index })?;
        let open = OpenItem::read(index, item)?;
        if !seen.insert(open.id_text) {
            return Err(OpenItemsError::Duplicate { index, id: open.id });
        }
        if open.branch != Some(branch) {
            continue;
        }
        match standing(open.triage, open.blocks) {
            Standing::Clear => {}
            Standing::PendingTriage => checkpoint.pending.push(open.id),
            Standing::Blocking(causes) => checkpoint.blocking.push(BlockingItem {
                id: open.id,
                causes,
            }),
        }
    }
    Ok(checkpoint)
}

/// The fields of one listed item that CP1 reads.
struct OpenItem<'a> {
    id: WeighInId,
    /// The id as listed, for the duplicate check.
    id_text: &'a str,
    branch: Option<&'a str>,
    triage: Option<Triage>,
    blocks: Blocks,
}

impl<'a> OpenItem<'a> {
    /// Reads item `index`, refusing any field assay reads that is missing or not what weighin
    /// writes.
    fn read(index: usize, item: &'a Map<String, Value>) -> Result<Self, OpenItemsError> {
        let fields = Fields { index, item };
        let id_text = fields
            .nullable_str("id")?
            .ok_or_else(|| fields.refused("id"))?;
        let id = id_text
            .parse()
            .map_err(|source| OpenItemsError::Id { index, source })?;
        let status = fields.nullable_str("status")?;
        if !status.is_some_and(|status| OPEN_STATUSES.contains(&status)) {
            return Err(fields.refused("status"));
        }
        let door = fields.nullable_code("door", &Door::ALL, Door::code)?;
        let authority = fields.nullable_code("authority", &Authority::ALL, Authority::code)?;
        let blocks = match fields.nullable_str("blocks")? {
            None => Blocks::Unstated,
            Some(_) => Blocks::Stated,
        };
        Ok(Self {
            id,
            id_text,
            branch: fields.nullable_str("branch")?,
            triage: door
                .zip(authority)
                .map(|(door, authority)| Triage { door, authority }),
            blocks,
        })
    }
}

/// The fields of item `index`, each read as weighin writes it or refused.
#[derive(Clone, Copy)]
struct Fields<'a> {
    index: usize,
    item: &'a Map<String, Value>,
}

impl<'a> Fields<'a> {
    /// The refusal of `field`.
    const fn refused(self, field: &'static str) -> OpenItemsError {
        OpenItemsError::Field {
            index: self.index,
            field,
        }
    }

    /// `field`, a string or null.
    fn nullable_str(self, field: &'static str) -> Result<Option<&'a str>, OpenItemsError> {
        match self.item.get(field) {
            Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => Ok(Some(text)),
            _ => Err(self.refused(field)),
        }
    }

    /// `field`, null or the `code` of one of `codes`.
    fn nullable_code<T: Copy>(
        self,
        field: &'static str,
        codes: &[T],
        code: impl Fn(T) -> &'static str,
    ) -> Result<Option<T>, OpenItemsError> {
        let Some(text) = self.nullable_str(field)? else {
            return Ok(None);
        };
        codes
            .iter()
            .copied()
            .find(|candidate| code(*candidate) == text)
            .map(Some)
            .ok_or_else(|| self.refused(field))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// `weighin open --repo --json` from weighin 0.9.0 on a fixture repository: on `feat-x`, a
    /// triaged one-way authority question (`wi-7529b9`), an untriaged one (`wi-b83949`) and a
    /// triaged two-way judgment call (`wi-567b42`); on `other`, an untriaged one (`wi-216116`).
    const RECORDED: &[u8] = include_bytes!("../tests/fixtures/weigh-in/open-0.9.0.json");

    fn id(text: &str) -> WeighInId {
        text.parse().expect("a fixture id")
    }

    fn recorded() -> Vec<Value> {
        match serde_json::from_slice(RECORDED).expect("the fixture is JSON") {
            Value::Array(items) => items,
            other => panic!("the fixture is a list, not {other}"),
        }
    }

    fn with(items: &[Value]) -> Vec<u8> {
        serde_json::to_vec(items).expect("a value encodes")
    }

    /// The recorded items with `field` of the first one set to `value`.
    fn first_with(field: &str, value: Value) -> Vec<u8> {
        let mut items = recorded();
        items[0][field] = value;
        with(&items)
    }

    #[test]
    fn the_recorded_branch_has_one_blocking_and_one_pending_item() {
        let checkpoint = checkpoint(RECORDED, "feat-x").expect("the recorded output is read");

        assert_eq!(
            shown(checkpoint.blocking()),
            [(
                id("wi-7529b9"),
                vec![BlockingCause::Door(Door::OneWay), BlockingCause::Authority],
            )]
        );
        assert_eq!(checkpoint.pending(), [id("wi-b83949")]);
        assert!(checkpoint.blocks_push());
    }

    #[test]
    fn items_on_other_branches_do_not_count() {
        let checkpoint = checkpoint(RECORDED, "other").expect("the recorded output is read");

        assert_eq!(checkpoint.blocking(), []);
        assert_eq!(checkpoint.pending(), [id("wi-216116")]);
        assert!(!checkpoint.blocks_push(), "pending triage only warns");
    }

    #[test]
    fn a_stated_blocks_makes_a_triaged_two_way_judgment_call_block() {
        let mut items = recorded();
        items[3]["blocks"] = json!("merge");

        let checkpoint = checkpoint(&with(&items), "feat-x").expect("read");

        assert_eq!(
            shown(checkpoint.blocking()).get(1),
            Some(&(id("wi-567b42"), vec![BlockingCause::Blocks]))
        );
    }

    /// Each blocking item's id and causes.
    fn shown(items: &[BlockingItem]) -> Vec<(WeighInId, Vec<BlockingCause>)> {
        items
            .iter()
            .map(|item| (item.id().clone(), item.causes().iter().collect()))
            .collect()
    }

    #[test]
    fn an_empty_list_is_nothing_open() {
        let checkpoint = checkpoint(b"[]\n", "main").expect("read");

        assert_eq!(
            (checkpoint.blocking(), checkpoint.pending()),
            (&[][..], &[][..])
        );
        assert_eq!(checkpoint.render(), "weigh-in on main: nothing open\n");
    }

    #[test]
    fn the_report_names_each_item_and_what_the_push_needs() {
        let checkpoint = checkpoint(RECORDED, "feat-x").expect("read");

        assert_eq!(
            checkpoint.render(),
            "weigh-in on feat-x: 1 blocking, 1 pending triage\n\
             \x20 blocking wi-7529b9: door one-way, authority\n\
             \x20 pending triage wi-b83949\n\
             \x20 push blocked: settle the blocking items with /weigh-in (git push --no-verify \
             skips this check)\n"
        );
    }

    #[test]
    fn pending_items_alone_warn() {
        let checkpoint = checkpoint(RECORDED, "other").expect("read");

        assert_eq!(
            checkpoint.render(),
            "weigh-in on other: 0 blocking, 1 pending triage\n\
             \x20 pending triage wi-216116\n\
             \x20 warning: triage the pending items with /weigh-in before the push\n"
        );
    }

    #[test]
    fn the_branch_is_shown_as_a_plain_name() {
        let checkpoint = checkpoint(b"[]", "a\x1b[2Jb").expect("read");

        assert_eq!(checkpoint.render(), "weigh-in on a??2Jb: nothing open\n");
    }

    #[test]
    fn output_past_the_byte_cap_is_refused_unparsed() {
        let over = vec![b' '; MAX_OPEN_BYTES + 1];

        assert!(matches!(
            checkpoint(&over, "main"),
            Err(OpenItemsError::TooLarge { len }) if len == MAX_OPEN_BYTES + 1
        ));
    }

    #[test]
    fn output_that_is_not_a_json_list_is_refused() {
        assert!(matches!(
            checkpoint(b"[{", "main"),
            Err(OpenItemsError::Json(_))
        ));
        assert!(matches!(
            checkpoint(b"{\"items\":[]}", "main"),
            Err(OpenItemsError::NotAList)
        ));
    }

    #[test]
    fn a_repeated_key_is_refused() {
        let text = br#"[{"id":"wi-7529b9","id":"wi-b83949"}]"#;

        assert!(matches!(
            checkpoint(text, "main"),
            Err(OpenItemsError::Json(_))
        ));
    }

    #[test]
    fn more_items_than_the_cap_is_refused_as_possibly_truncated() {
        let item = recorded().swap_remove(1);
        let mut items = vec![item; MAX_OPEN_ITEMS + 1];
        for (n, item) in items.iter_mut().enumerate() {
            item["id"] = json!(format!("wi-{n:04x}"));
        }

        assert!(matches!(
            checkpoint(&with(&items), "main"),
            Err(OpenItemsError::TooMany)
        ));
        items.pop();
        assert!(
            checkpoint(&with(&items), "main").is_ok(),
            "the cap itself is read"
        );
    }

    #[test]
    fn a_field_assay_reads_that_is_missing_or_of_another_shape_is_refused() {
        let cases = [
            ("branch", json!(1)),
            ("status", json!("decided")),
            ("status", json!("Triaged")),
            ("status", Value::Null),
            ("door", json!("sideways")),
            ("door", json!(true)),
            ("authority", json!("agent")),
            ("blocks", json!(["merge"])),
            ("id", json!(7)),
        ];
        for (field, value) in cases {
            let refused = checkpoint(&first_with(field, value.clone()), "feat-x");
            assert!(
                matches!(&refused, Err(OpenItemsError::Field { index: 0, field: named }) if *named == field),
                "{field} = {value}: {refused:?}"
            );
        }
        for field in ["id", "status", "branch", "door", "authority", "blocks"] {
            let mut items = recorded();
            items[0]
                .as_object_mut()
                .expect("an item is an object")
                .remove(field);
            let refused = checkpoint(&with(&items), "feat-x");
            assert!(
                matches!(&refused, Err(OpenItemsError::Field { index: 0, field: named }) if *named == field),
                "without {field}: {refused:?}"
            );
        }
    }

    #[test]
    fn an_item_that_is_not_an_object_is_refused() {
        assert!(matches!(
            checkpoint(b"[[]]", "main"),
            Err(OpenItemsError::NotAnItem { index: 0 })
        ));
    }

    #[test]
    fn an_id_weighin_would_not_write_is_refused() {
        let refused = checkpoint(&first_with("id", json!("wi-7529B9")), "feat-x");

        assert!(
            matches!(
                refused,
                Err(OpenItemsError::Id {
                    index: 0,
                    source: ParseWeighInIdError::Digit { index: 7 }
                })
            ),
            "{refused:?}"
        );
    }

    #[test]
    fn an_id_listed_twice_is_refused() {
        let items = recorded();
        let doubled = [items.clone(), items].concat();

        assert!(matches!(
            checkpoint(&with(&doubled), "feat-x"),
            Err(OpenItemsError::Duplicate { index: 4, .. })
        ));
    }

    #[test]
    fn a_half_triaged_item_is_pending_not_clear() {
        let mut items = recorded();
        items[3]["authority"] = Value::Null;

        let checkpoint = checkpoint(&with(&items), "feat-x").expect("read");

        assert_eq!(checkpoint.pending(), [id("wi-b83949"), id("wi-567b42")]);
    }
}
