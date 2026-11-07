//! The items the head's journals record, and the two sets of 04 §9 rule 1 over them.
//!
//! Events of every readable journal are ordered by `ts`, then journal path, then line (an item
//! is raised in one session's journal and may be triaged or settled in another's). Per item,
//! the last status event decides its status, the last `item_triaged` its triaged `door` and
//! `authority`, and the last `item_raised` its `blocks` and where it is reported. An item with
//! no status event (only `record_written`) has an unknown status and counts as open; one with no
//! `item_raised` blocks on something unknown (missing is never none) and is reported at its
//! first event.

use std::collections::HashMap;
use std::num::NonZeroU64;

use super::id::ItemId;
use super::journal::{Blocks, Change, Event, Status};

/// The `door` of a triaged item that does not block by itself.
pub(super) const TWO_WAY: &str = "two-way";
/// The `authority` of a triaged item that does not block by itself.
pub(super) const JUDGMENT: &str = "judgment";

/// One journal's path and events.
#[derive(Debug)]
pub(super) struct Journal<'a> {
    pub(super) path: &'a str,
    pub(super) events: Vec<Event>,
}

/// One item as the journals leave it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Item<'a> {
    pub(super) id: &'a ItemId,
    /// `None` when no event set one.
    pub(super) status: Option<Status>,
    /// The last triage's `door` and `authority`; both `None` when never triaged.
    pub(super) door: Option<&'a str>,
    pub(super) authority: Option<&'a str>,
    pub(super) blocks: Blocks,
    /// The journal and line it is reported at.
    pub(super) path: &'a str,
    pub(super) line: NonZeroU64,
}

/// Where an item stands under 04 §9 rule 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Standing<'a> {
    /// Closed, or open, triaged, two-way, judgment-bound and blocking nothing.
    Settled,
    /// Open with its triaged `door` or `authority` still null.
    PendingTriage,
    /// Open and triaged, with `blocks` not none, `door` not two-way or `authority` not
    /// judgment-bound. Any value other than `two-way` and `judgment` blocks.
    Blocking { door: &'a str, authority: &'a str },
}

impl<'a> Item<'a> {
    /// A fresh item first seen at `line` of the journal at `path`.
    fn new(id: &'a ItemId, path: &'a str, line: NonZeroU64) -> Self {
        Self {
            id,
            status: None,
            door: None,
            authority: None,
            blocks: Blocks::Unknown,
            path,
            line,
        }
    }

    /// Whether it is open; an unknown status is open.
    pub(super) fn is_open(&self) -> bool {
        self.status.is_none_or(Status::is_open)
    }

    /// Where it stands.
    pub(super) fn standing(&self) -> Standing<'a> {
        if !self.is_open() {
            return Standing::Settled;
        }
        let (Some(door), Some(authority)) = (self.door, self.authority) else {
            return Standing::PendingTriage;
        };
        if self.blocks == Blocks::Nothing && door == TWO_WAY && authority == JUDGMENT {
            Standing::Settled
        } else {
            Standing::Blocking { door, authority }
        }
    }

    /// Applies `event`, read from the journal at `path`.
    fn apply(&mut self, event: &'a Event, path: &'a str) {
        if let Some(status) = event.change.status() {
            self.status = Some(status);
        }
        match &event.change {
            Change::Raised(blocks) => {
                self.blocks = *blocks;
                self.path = path;
                self.line = event.line;
            }
            Change::Triaged { door, authority } => {
                self.door = door.as_deref();
                self.authority = authority.as_deref();
            }
            Change::Moved(_) | Change::Recorded => {}
        }
    }
}

/// Every item `journals` record, ordered by where it is reported.
pub(super) fn items<'a>(journals: &'a [Journal<'a>]) -> Vec<Item<'a>> {
    let mut events: Vec<(&str, &Event)> = journals
        .iter()
        .flat_map(|journal| journal.events.iter().map(|event| (journal.path, event)))
        .collect();
    events.sort_by(|(left_path, left), (right_path, right)| {
        left.ts
            .cmp(&right.ts)
            .then_with(|| left_path.cmp(right_path))
            .then(left.line.cmp(&right.line))
    });
    let mut items: HashMap<&ItemId, Item<'_>> = HashMap::new();
    for (path, event) in events {
        items
            .entry(&event.item)
            .or_insert_with(|| Item::new(&event.item, path, event.line))
            .apply(event, path);
    }
    let mut items: Vec<Item<'_>> = items.into_values().collect();
    items.sort_by(|left, right| {
        (left.path, left.line, left.id).cmp(&(right.path, right.line, right.id))
    });
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(text: &str) -> ItemId {
        ItemId::new(text.to_owned()).expect("a valid id")
    }

    fn event(item: &str, ts: &str, line: u64, change: Change) -> Event {
        Event {
            item: id(item),
            ts: ts.to_owned(),
            line: NonZeroU64::new(line).expect("a line number"),
            change,
        }
    }

    fn triaged(door: &str, authority: &str) -> Change {
        Change::Triaged {
            door: Some(door.to_owned()),
            authority: Some(authority.to_owned()),
        }
    }

    #[test]
    fn a_later_event_in_another_journal_decides_the_status() {
        let journals = [
            Journal {
                path: "b.jsonl",
                events: vec![event(
                    "wi-aaaa",
                    "2026-10-12T00:00:00Z",
                    1,
                    Change::Moved(Status::Decided),
                )],
            },
            Journal {
                path: "a.jsonl",
                events: vec![
                    event(
                        "wi-aaaa",
                        "2026-10-11T00:00:00Z",
                        1,
                        Change::Raised(Blocks::Nothing),
                    ),
                    event(
                        "wi-aaaa",
                        "2026-10-11T00:00:01Z",
                        2,
                        triaged("one-way", "authority"),
                    ),
                ],
            },
        ];

        let items = items(&journals);

        assert_eq!(
            items
                .iter()
                .map(|item| (item.path, item.line.get(), item.status, item.standing()))
                .collect::<Vec<_>>(),
            [("a.jsonl", 1, Some(Status::Decided), Standing::Settled)]
        );
    }

    #[test]
    fn an_item_with_only_a_record_is_open_and_untriaged() {
        let journals = [Journal {
            path: "a.jsonl",
            events: vec![event(
                "wi-aaaa",
                "2026-10-11T00:00:00Z",
                3,
                Change::Recorded,
            )],
        }];

        let items = items(&journals);

        assert_eq!(
            items
                .iter()
                .map(|item| (item.line.get(), item.status, item.standing()))
                .collect::<Vec<_>>(),
            [(3, None, Standing::PendingTriage)]
        );
    }

    #[test]
    fn a_triaged_item_never_raised_blocks_on_unknown_blocks() {
        let journals = [Journal {
            path: "a.jsonl",
            events: vec![event(
                "wi-aaaa",
                "2026-10-11T00:00:00Z",
                1,
                triaged(TWO_WAY, JUDGMENT),
            )],
        }];

        let items = items(&journals);

        assert_eq!(
            items.iter().map(Item::standing).collect::<Vec<_>>(),
            [Standing::Blocking {
                door: TWO_WAY,
                authority: JUDGMENT
            }]
        );
    }
}
