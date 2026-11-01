//! decide-only's per-actor command counter (D6 A, J7, T9, T12): 20 commands per actor per PR,
//! every `/assay` line counted before anything else, privileged ids counted but never limited.

use std::error::Error;

use assay_app::{
    ActorCounter, CommandCount, Counted, Exempt, LedgerPath, MAX_COMMANDS_PER_ACTOR,
    MAX_STATE_ENTRIES, PrState, UserId, WriteOutcome, count_command, read_state, update_state,
};

use crate::ledger_fakes::{MemoryLedger, id};

const ACTOR: u64 = 1001;

fn counted(actor: u64, count: u32) -> ActorCounter {
    ActorCounter {
        actor_id: UserId(id(actor)),
        count,
    }
}

/// A ledger whose state for PR 7 of repository 42 holds `counters`.
fn ledger_with(counters: Vec<ActorCounter>) -> Result<MemoryLedger, Box<dyn Error>> {
    let ledger = MemoryLedger::default();
    let state = PrState {
        actor_counters: counters,
        ..PrState::default()
    };
    update_state(&ledger, id(42), id(7), |_| Some(state.clone()))?;
    Ok(ledger)
}

fn count(ledger: &MemoryLedger, actor: u64, exempt: Exempt) -> Result<Counted, Box<dyn Error>> {
    Ok(count_command(
        ledger,
        CommandCount {
            repo_id: id(42),
            pr: id(7),
            actor: UserId(id(actor)),
            exempt,
        },
    )?)
}

fn counters(ledger: &MemoryLedger) -> Result<Vec<ActorCounter>, Box<dyn Error>> {
    Ok(read_state(ledger, id(42), id(7))?.actor_counters)
}

#[test]
fn the_first_command_of_an_actor_writes_a_count_of_one() -> Result<(), Box<dyn Error>> {
    let ledger = MemoryLedger::default();

    let counted_now = count(&ledger, ACTOR, Exempt::No)?;

    assert_eq!(counted_now, Counted::Within { count: 1 });
    assert_eq!(counters(&ledger)?, [counted(ACTOR, 1)]);
    Ok(())
}

#[test]
fn the_twentieth_command_is_within_the_limit() -> Result<(), Box<dyn Error>> {
    let ledger = ledger_with(vec![counted(ACTOR, 19)])?;

    let counted_now = count(&ledger, ACTOR, Exempt::No)?;

    assert_eq!(counted_now, Counted::Within { count: 20 });
    assert_eq!(MAX_COMMANDS_PER_ACTOR, 20);
    assert_eq!(counters(&ledger)?, [counted(ACTOR, 20)]);
    Ok(())
}

#[test]
fn the_twenty_first_command_is_limited_and_still_counted() -> Result<(), Box<dyn Error>> {
    let ledger = ledger_with(vec![counted(ACTOR, 20)])?;

    let counted_now = count(&ledger, ACTOR, Exempt::No)?;

    assert_eq!(counted_now, Counted::Limited { count: 21 });
    assert_eq!(counters(&ledger)?, [counted(ACTOR, 21)]);
    Ok(())
}

#[test]
fn a_privileged_actor_is_never_limited_and_is_counted() -> Result<(), Box<dyn Error>> {
    let ledger = ledger_with(vec![counted(ACTOR, 20)])?;

    let counted_now = count(&ledger, ACTOR, Exempt::Privileged)?;

    assert_eq!(counted_now, Counted::Within { count: 21 });
    assert_eq!(counters(&ledger)?, [counted(ACTOR, 21)]);
    Ok(())
}

#[test]
fn only_the_actor_counted_changes() -> Result<(), Box<dyn Error>> {
    let ledger = ledger_with(vec![counted(2002, 5), counted(ACTOR, 3)])?;

    let counted_now = count(&ledger, ACTOR, Exempt::No)?;

    assert_eq!(counted_now, Counted::Within { count: 4 });
    assert_eq!(counters(&ledger)?, [counted(2002, 5), counted(ACTOR, 4)]);
    Ok(())
}

/// A state holding `MAX_STATE_ENTRIES` counters, none of them for [`ACTOR`].
fn full_ledger() -> Result<MemoryLedger, Box<dyn Error>> {
    let cap = u64::try_from(MAX_STATE_ENTRIES)?;
    ledger_with((1..=cap).map(|actor| counted(actor, 1)).collect())
}

#[test]
fn a_new_actor_past_the_counter_cap_is_limited_with_nothing_written() -> Result<(), Box<dyn Error>>
{
    let ledger = full_ledger()?;
    let before = ledger.writes().len();

    let counted_now = count(&ledger, ACTOR, Exempt::No)?;

    assert_eq!(counted_now, Counted::Limited { count: 0 });
    assert_eq!(ledger.writes().len(), before);
    Ok(())
}

#[test]
fn a_new_privileged_actor_past_the_counter_cap_is_within_with_nothing_written()
-> Result<(), Box<dyn Error>> {
    let ledger = full_ledger()?;
    let before = ledger.writes().len();

    let counted_now = count(&ledger, ACTOR, Exempt::Privileged)?;

    assert_eq!(counted_now, Counted::Within { count: 0 });
    assert_eq!(ledger.writes().len(), before);
    Ok(())
}

#[test]
fn a_counted_actor_in_a_full_state_is_still_counted() -> Result<(), Box<dyn Error>> {
    let ledger = full_ledger()?;

    let counted_now = count(&ledger, 1, Exempt::No)?;

    assert_eq!(counted_now, Counted::Within { count: 2 });
    assert_eq!(counters(&ledger)?.first(), Some(&counted(1, 2)));
    Ok(())
}

#[test]
fn a_count_at_its_maximum_stays_there_and_is_limited() -> Result<(), Box<dyn Error>> {
    let ledger = ledger_with(vec![counted(ACTOR, u32::MAX)])?;

    let counted_now = count(&ledger, ACTOR, Exempt::No)?;

    assert_eq!(counted_now, Counted::Limited { count: u32::MAX });
    assert_eq!(counters(&ledger)?, [counted(ACTOR, u32::MAX)]);
    Ok(())
}

#[test]
fn a_conflicting_write_is_retried_on_the_fresh_count() -> Result<(), Box<dyn Error>> {
    let rival = concat!(
        r#"{"schema":"assay.ledger-state/1","repo_id":42,"pr":7,"evidence_records":[],"#,
        r#""actor_counters":[{"actor_id":1001,"count":20}]}"#,
        "\n",
    );
    let ledger = MemoryLedger::default()
        .with_file(
            &LedgerPath::state(id(42), id(7)),
            concat!(
                r#"{"schema":"assay.ledger-state/1","repo_id":42,"pr":7,"evidence_records":[],"#,
                r#""actor_counters":[{"actor_id":1001,"count":3}]}"#,
                "\n",
            )
            .as_bytes(),
        )
        .rival_writes(rival.as_bytes());

    let counted_now = count(&ledger, ACTOR, Exempt::No)?;

    assert_eq!(counted_now, Counted::Limited { count: 21 });
    assert_eq!(counters(&ledger)?, [counted(ACTOR, 21)]);
    let outcomes: Vec<_> = ledger.writes().into_iter().map(|w| w.outcome).collect();
    assert_eq!(outcomes, [WriteOutcome::Conflict, WriteOutcome::Written]);
    Ok(())
}
