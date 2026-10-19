//! Weigh-In items as the checkpoints of `docs/architecture/04-pipeline.md` §9 read them: the
//! item id of `docs/decisions/0003-weigh-in-id-formats.md` and the triage rule that sorts an
//! open item into blocking, pending triage or clear (CP1, and CP2's rule 1).
//!
//! Pure: the adapters that read Weigh-In's output build these values and ask [`standing`].

use std::fmt;
use std::ops::RangeInclusive;
use std::str::FromStr;

use crate::vocabulary::closed_enum;

/// The prefix of every Weigh-In item id.
const PREFIX: &str = "wi-";

/// The fewest and most hex digits after [`PREFIX`] (`weighin_core::ItemId`, ADR 0003).
const DIGITS: RangeInclusive<usize> = 4..=16;

/// A Weigh-In item id: `wi-` then 4 to 16 lowercase hex digits.
///
/// Stricter than `weighin_core::ItemId::parse`, which trims surrounding whitespace first: an id
/// weighin wrote never has any, so one that does is refused rather than repaired (wi-b5affa).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WeighInId(String);

impl WeighInId {
    /// The id as weighin writes it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WeighInId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why a text is not a Weigh-In item id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ParseWeighInIdError {
    /// It does not start with `wi-`.
    #[error("a Weigh-In id starts with \"wi-\"")]
    Prefix,
    /// The digits after `wi-` are too few or too many.
    #[error("a Weigh-In id has 4 to 16 hex digits after \"wi-\", not {digits}")]
    Length {
        /// How many digits follow the prefix.
        digits: usize,
    },
    /// A byte after the prefix is not a lowercase hex digit.
    #[error("a Weigh-In id is lowercase hex after \"wi-\", but byte {index} is not")]
    Digit {
        /// The byte offset of the first such byte in the whole text.
        index: usize,
    },
}

impl FromStr for WeighInId {
    type Err = ParseWeighInIdError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let digits = text
            .strip_prefix(PREFIX)
            .ok_or(ParseWeighInIdError::Prefix)?;
        if let Some(offset) = digits
            .bytes()
            .position(|byte| !matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(ParseWeighInIdError::Digit {
                index: PREFIX.len().saturating_add(offset),
            });
        }
        if !DIGITS.contains(&digits.len()) {
            return Err(ParseWeighInIdError::Length {
                digits: digits.len(),
            });
        }
        Ok(Self(text.to_owned()))
    }
}

closed_enum! {
    /// How reversible a triaged item's decision is: Weigh-In's triaged `door`.
    pub enum Door {
        /// Hard or impossible to reverse.
        OneWay => "one-way",
        /// Cheap to reverse.
        TwoWay => "two-way",
        /// Not known.
        Unsure => "unsure",
    }
}

closed_enum! {
    /// Who may settle a triaged item: Weigh-In's triaged `authority`.
    pub enum Authority {
        /// Only someone with authority over the project (the owner).
        Authority => "authority",
        /// The agent's judgment may settle it.
        Judgment => "judgment",
    }
}

/// The triage of an item: set by `/weigh-in` triage, absent until then.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Triage {
    /// The triaged door.
    pub door: Door,
    /// The triaged authority.
    pub authority: Authority,
}

/// Whether the raising agent named something the item blocks (`blocks`, free text today).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Blocks {
    /// Nothing named (`blocks` is null).
    Unstated,
    /// Something named, whatever it says.
    Stated,
}

/// One reason a triaged item blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlockingCause {
    /// Its door is not two-way.
    Door(Door),
    /// Only the owner may settle it.
    Authority,
    /// It names something it blocks.
    Blocks,
}

/// Where an open item stands (04 §9).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Standing {
    /// Triaged, and nothing about it blocks.
    Clear,
    /// Not triaged yet: never treated as non-blocking, only as waiting.
    PendingTriage,
    /// Triaged and blocking, for these causes: never empty, in the order door, authority,
    /// blocks.
    Blocking(Vec<BlockingCause>),
}

/// Where an open item with `triage` and `blocks` stands (04 §9 CP1 and CP2 rule 1).
///
/// Pending triage until triaged; then blocking when its door is not two-way, its authority is
/// the owner's or it names what it blocks; else clear.
#[must_use]
pub fn standing(triage: Option<Triage>, blocks: Blocks) -> Standing {
    let Some(Triage { door, authority }) = triage else {
        return Standing::PendingTriage;
    };
    let causes: Vec<BlockingCause> = [
        (door != Door::TwoWay).then_some(BlockingCause::Door(door)),
        (authority == Authority::Authority).then_some(BlockingCause::Authority),
        (blocks == Blocks::Stated).then_some(BlockingCause::Blocks),
    ]
    .into_iter()
    .flatten()
    .collect();
    if causes.is_empty() {
        Standing::Clear
    } else {
        Standing::Blocking(causes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_id_is_wi_then_four_to_sixteen_lowercase_hex_digits() {
        for text in ["wi-1e9b", "wi-1e9b23", "wi-1e9b2345", "wi-0123456789abcdef"] {
            let id: WeighInId = text.parse().expect("a weighin id");
            assert_eq!(id.as_str(), text);
        }
    }

    #[test]
    fn an_id_without_the_prefix_of_the_wrong_length_or_with_other_digits_is_refused() {
        let cases = [
            ("1e9b23", ParseWeighInIdError::Prefix),
            ("WI-1e9b23", ParseWeighInIdError::Prefix),
            ("wi-1e9", ParseWeighInIdError::Length { digits: 3 }),
            (
                "wi-0123456789abcdef0",
                ParseWeighInIdError::Length { digits: 17 },
            ),
            ("wi-1E9b23", ParseWeighInIdError::Digit { index: 4 }),
            (" wi-1e9b23", ParseWeighInIdError::Prefix),
            ("wi-1e9b23\n", ParseWeighInIdError::Digit { index: 9 }),
        ];
        for (text, expected) in cases {
            assert_eq!(text.parse::<WeighInId>(), Err(expected), "{text:?}");
        }
    }

    const fn triaged(door: Door, authority: Authority) -> Triage {
        Triage { door, authority }
    }

    #[test]
    fn an_untriaged_item_is_pending_triage_even_when_it_names_what_it_blocks() {
        assert_eq!(standing(None, Blocks::Unstated), Standing::PendingTriage);
        assert_eq!(standing(None, Blocks::Stated), Standing::PendingTriage);
    }

    #[test]
    fn a_triaged_two_way_judgment_call_that_blocks_nothing_is_clear() {
        assert_eq!(
            standing(
                Some(triaged(Door::TwoWay, Authority::Judgment)),
                Blocks::Unstated
            ),
            Standing::Clear
        );
    }

    #[test]
    fn each_blocking_condition_blocks_on_its_own() {
        let cases = [
            (
                triaged(Door::OneWay, Authority::Judgment),
                Blocks::Unstated,
                vec![BlockingCause::Door(Door::OneWay)],
            ),
            (
                triaged(Door::Unsure, Authority::Judgment),
                Blocks::Unstated,
                vec![BlockingCause::Door(Door::Unsure)],
            ),
            (
                triaged(Door::TwoWay, Authority::Authority),
                Blocks::Unstated,
                vec![BlockingCause::Authority],
            ),
            (
                triaged(Door::TwoWay, Authority::Judgment),
                Blocks::Stated,
                vec![BlockingCause::Blocks],
            ),
        ];
        for (triage, blocks, causes) in cases {
            assert_eq!(standing(Some(triage), blocks), Standing::Blocking(causes));
        }
    }

    #[test]
    fn every_cause_is_listed_in_order_door_authority_blocks() {
        assert_eq!(
            standing(
                Some(triaged(Door::OneWay, Authority::Authority)),
                Blocks::Stated
            ),
            Standing::Blocking(vec![
                BlockingCause::Door(Door::OneWay),
                BlockingCause::Authority,
                BlockingCause::Blocks,
            ])
        );
    }

    #[test]
    fn the_triage_vocabularies_are_weighins_codes() {
        assert_eq!(Door::ALL.map(Door::code), ["one-way", "two-way", "unsure"]);
        assert_eq!(
            Authority::ALL.map(Authority::code),
            ["authority", "judgment"]
        );
    }
}
