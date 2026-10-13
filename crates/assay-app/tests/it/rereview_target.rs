//! What a re-review reviews and carries (`docs/architecture/04-pipeline.md` §7, #99): the
//! cumulative target with the increment marked from `git range-diff`, and the carry-over kept
//! only while the previous head is an ancestor of head (fixtures for a force-push and an
//! unreachable previous head; the forge's git fixtures are in `assay-forge`).

use std::cell::RefCell;
use std::error::Error;
use std::fmt;
use std::num::NonZeroU64;

use assay_app::{
    Ancestry, CarriedFinding, CarriedParts, Carry, CarryReset, CommitId, Cumulative, Increment,
    MAX_RANGE_DIFF_BYTES, PriorHistory, RangeDiffCommits, RangeDiffListing, ReviewTarget,
    TargetRequest, review_target,
};
use assay_domain::{Class, Severity};

/// The history could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Broken;

impl fmt::Display for Broken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("git failed")
    }
}

impl Error for Broken {}

/// One call the fake was given, with its arguments' first hex digits.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    Ancestry {
        previous: char,
        head: char,
    },
    RangeDiff {
        base: char,
        previous: char,
        head: char,
        limit: usize,
    },
}

/// A history answering each call as scripted and recording it.
struct History {
    ancestry: Result<Ancestry, Broken>,
    range_diff: Result<RangeDiffListing, Broken>,
    calls: RefCell<Vec<Call>>,
}

impl History {
    fn new(ancestry: Ancestry, range_diff: RangeDiffListing) -> Self {
        Self {
            ancestry: Ok(ancestry),
            range_diff: Ok(range_diff),
            calls: RefCell::new(Vec::new()),
        }
    }
}

fn first(commit: &CommitId) -> char {
    commit.as_str().chars().next().unwrap_or('?')
}

impl PriorHistory for History {
    type Error = Broken;

    fn ancestry(&self, previous: &CommitId, head: &CommitId) -> Result<Ancestry, Broken> {
        self.calls.borrow_mut().push(Call::Ancestry {
            previous: first(previous),
            head: first(head),
        });
        self.ancestry
    }

    fn range_diff(
        &self,
        commits: RangeDiffCommits<'_>,
        limit: usize,
    ) -> Result<RangeDiffListing, Broken> {
        self.calls.borrow_mut().push(Call::RangeDiff {
            base: first(commits.base),
            previous: first(commits.previous),
            head: first(commits.head),
            limit,
        });
        // A copy: the script answers every call alike.
        self.range_diff.clone()
    }
}

/// The commit whose 40 hex digits are all `digit`.
fn commit(digit: char) -> Result<CommitId, Box<dyn Error>> {
    Ok(digit.to_string().repeat(40).parse()?)
}

/// `review_target` for base `b`, head `d` and, if given, previous head `c`.
fn target(history: &History, previous: bool) -> Result<ReviewTarget<Broken>, Box<dyn Error>> {
    let (base, head, prior) = (commit('b')?, commit('d')?, commit('c')?);
    let request = TargetRequest {
        base: &base,
        head: &head,
        previous: previous.then_some(&prior),
    };
    Ok(review_target(&request, history)?)
}

const RANGE_DIFF: &[u8] = b"1:  abc1234 = 1:  def5678 subject\n";

/// The range-diff text a `Since` increment marks, and from which commit.
fn since(increment: &Increment) -> Option<(char, &str)> {
    match increment {
        Increment::Since {
            previous,
            range_diff,
        } => Some((first(previous), range_diff.as_str())),
        Increment::Cumulative(_) => None,
    }
}

#[test]
fn the_first_review_marks_nothing_carries_nothing_and_reads_no_history()
-> Result<(), Box<dyn Error>> {
    let history = History::new(Ancestry::Ancestor, RangeDiffListing::TooLarge);

    let target = target(&history, false)?;

    assert_eq!(
        (target.increment(), target.carry()),
        (
            &Increment::Cumulative(Cumulative::FirstReview),
            Carry::FirstReview
        )
    );
    assert_eq!(history.calls.borrow().as_slice(), []);
    Ok(())
}

#[test]
fn a_push_on_top_marks_the_range_diff_and_keeps_the_carry_over() -> Result<(), Box<dyn Error>> {
    let history = History::new(
        Ancestry::Ancestor,
        RangeDiffListing::Listed(RANGE_DIFF.to_vec()),
    );

    let target = target(&history, true)?;

    assert_eq!(
        since(target.increment()),
        Some(('c', "1:  abc1234 = 1:  def5678 subject\n"))
    );
    assert_eq!(target.carry(), Carry::Kept);
    assert_eq!(
        history.calls.borrow().as_slice(),
        [
            Call::Ancestry {
                previous: 'c',
                head: 'd'
            },
            Call::RangeDiff {
                base: 'b',
                previous: 'c',
                head: 'd',
                limit: MAX_RANGE_DIFF_BYTES
            },
        ]
    );
    Ok(())
}

#[test]
fn a_force_push_marks_the_range_diff_and_resets_the_carry_over() -> Result<(), Box<dyn Error>> {
    let history = History::new(
        Ancestry::Rewritten,
        RangeDiffListing::Listed(RANGE_DIFF.to_vec()),
    );

    let target = target(&history, true)?;

    assert_eq!(
        since(target.increment()),
        Some(('c', "1:  abc1234 = 1:  def5678 subject\n"))
    );
    assert_eq!(target.carry(), Carry::Reset(CarryReset::Rewritten));
    Ok(())
}

#[test]
fn an_unreachable_previous_head_reviews_cumulatively_and_resets_without_a_range_diff()
-> Result<(), Box<dyn Error>> {
    let history = History::new(
        Ancestry::Unreachable,
        RangeDiffListing::Listed(RANGE_DIFF.to_vec()),
    );

    let target = target(&history, true)?;

    assert_eq!(
        (target.increment(), target.carry()),
        (
            &Increment::Cumulative(Cumulative::Unreachable),
            Carry::Reset(CarryReset::Unreachable)
        )
    );
    assert_eq!(
        history.calls.borrow().len(),
        1,
        "no range-diff is asked for"
    );
    Ok(())
}

#[test]
fn a_range_diff_past_the_adapters_limit_leaves_the_target_unmarked_and_the_carry_kept()
-> Result<(), Box<dyn Error>> {
    let history = History::new(Ancestry::Ancestor, RangeDiffListing::TooLarge);

    let target = target(&history, true)?;

    assert_eq!(
        (target.increment(), target.carry()),
        (&Increment::Cumulative(Cumulative::Unavailable), Carry::Kept)
    );
    Ok(())
}

#[test]
fn a_range_diff_at_the_cap_is_marked_and_one_byte_more_is_not() -> Result<(), Box<dyn Error>> {
    let at_cap = History::new(
        Ancestry::Ancestor,
        RangeDiffListing::Listed(vec![b'x'; MAX_RANGE_DIFF_BYTES]),
    );
    let over = History::new(
        Ancestry::Ancestor,
        RangeDiffListing::Listed(vec![b'x'; MAX_RANGE_DIFF_BYTES + 1]),
    );

    let (at_cap, over) = (target(&at_cap, true)?, target(&over, true)?);

    assert_eq!(
        since(at_cap.increment()).map(|(_, text)| text.len()),
        Some(MAX_RANGE_DIFF_BYTES)
    );
    assert_eq!(
        over.increment(),
        &Increment::Cumulative(Cumulative::Unavailable)
    );
    Ok(())
}

#[test]
fn a_range_diff_that_is_not_utf8_leaves_the_target_unmarked() -> Result<(), Box<dyn Error>> {
    let history = History::new(
        Ancestry::Ancestor,
        RangeDiffListing::Listed(vec![b'a', 0xff, b'b']),
    );

    let target = target(&history, true)?;

    assert_eq!(
        (target.increment(), target.carry()),
        (&Increment::Cumulative(Cumulative::Unavailable), Carry::Kept)
    );
    Ok(())
}

#[test]
fn a_failed_ancestry_read_is_an_error_naming_it() -> Result<(), Box<dyn Error>> {
    let history = History {
        ancestry: Err(Broken),
        ..History::new(Ancestry::Ancestor, RangeDiffListing::TooLarge)
    };
    let (base, head, prior) = (commit('b')?, commit('d')?, commit('c')?);

    let failed = review_target(
        &TargetRequest {
            base: &base,
            head: &head,
            previous: Some(&prior),
        },
        &history,
    );

    let error = failed.err().ok_or("a broken history was read")?;
    assert_eq!(error.source, Broken);
    Ok(())
}

#[test]
fn a_failed_range_diff_leaves_the_target_unmarked_and_keeps_the_carry_over()
-> Result<(), Box<dyn Error>> {
    let history = History {
        range_diff: Err(Broken),
        ..History::new(Ancestry::Ancestor, RangeDiffListing::TooLarge)
    };

    let target = target(&history, true)?;

    assert_eq!(
        (
            target.increment(),
            target.carry(),
            target.range_diff_failure()
        ),
        (
            &Increment::Cumulative(Cumulative::Unavailable),
            Carry::Kept,
            Some(&Broken)
        )
    );
    Ok(())
}

#[test]
fn a_failed_range_diff_after_a_force_push_still_resets_the_carry_over() -> Result<(), Box<dyn Error>>
{
    let history = History {
        range_diff: Err(Broken),
        ..History::new(Ancestry::Rewritten, RangeDiffListing::TooLarge)
    };

    let target = target(&history, true)?;

    assert_eq!(
        (target.increment(), target.carry()),
        (
            &Increment::Cumulative(Cumulative::Unavailable),
            Carry::Reset(CarryReset::Rewritten)
        )
    );
    Ok(())
}

#[test]
fn a_range_diff_too_slow_to_finish_leaves_the_target_unmarked() -> Result<(), Box<dyn Error>> {
    let history = History::new(Ancestry::Ancestor, RangeDiffListing::TooSlow);

    let target = target(&history, true)?;

    assert_eq!(
        (
            target.increment(),
            target.carry(),
            target.range_diff_failure()
        ),
        (
            &Increment::Cumulative(Cumulative::Unavailable),
            Carry::Kept,
            None
        )
    );
    Ok(())
}

/// One carried finding, fingerprinted `n`.
fn open(n: usize) -> Result<CarriedFinding, Box<dyn Error>> {
    Ok(CarriedFinding::read(
        CarriedParts {
            fp: format!("{n:016x}").parse()?,
            file: "src/lib.rs".to_owned(),
            line: NonZeroU64::new(3).ok_or("3 is not zero")?,
            class: Class::Logic,
            severity: Severity::Major,
            title: "off by one in the loop bound".to_owned(),
            trigger: None,
            evidence: "for i in 0..=len {".to_owned(),
        },
        "Wet-Ink-Corporation/assay",
    )?)
}

#[test]
fn only_a_kept_carry_over_carries_the_open_findings() -> Result<(), Box<dyn Error>> {
    let mut carried = Vec::new();
    for (ancestry, previous) in [
        (Ancestry::Ancestor, true),
        (Ancestry::Ancestor, false),
        (Ancestry::Rewritten, true),
        (Ancestry::Unreachable, true),
    ] {
        let history = History::new(ancestry, RangeDiffListing::TooLarge);
        let target = target(&history, previous)?;
        carried.push(target.carried(vec![open(0)?, open(1)?]).len());
    }

    assert_eq!(carried, [2, 0, 0, 0]);
    Ok(())
}
