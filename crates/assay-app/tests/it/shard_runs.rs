//! Sharded runs (`docs/architecture/04-pipeline.md` §3, issue #98): shards run one after
//! another, each with the full specialist set over its own pack, so a run never has more than
//! four model processes; a paused or stopped stage skips the shards after it.

use std::collections::BTreeSet;
use std::error::Error;
use std::path::PathBuf;
use std::time::Duration;

use assay_app::{
    Halt, ModelOutcome, PromptBundle, PromptError, Role, RoleFailure, RoleOutcome,
    ShardedSpecialists, ShardsError, ShardsRequest, Specialists, run_shards,
};
use assay_domain::{Aeu, RequiredRole};

use crate::specialists_fakes::{
    Answer, Fake, Hold, SCHEMA, Step, Unreachable, bundle, document_from, ended, ran, tenth_of,
};

/// A head start that never passes in a test: the first call always ends before it.
const LONG: Duration = Duration::from_secs(3_600);

const FULL: [RequiredRole; 3] = [
    RequiredRole::Bugs,
    RequiredRole::Security,
    RequiredRole::Design,
];

const FULL_META: [RequiredRole; 4] = [
    RequiredRole::Bugs,
    RequiredRole::Security,
    RequiredRole::Design,
    RequiredRole::Meta,
];

/// A request for `roles` over `packs` with the stand-in bundle, no intent and `head_start`.
fn request<'a>(
    packs: &'a [String],
    roles: &'a [RequiredRole],
    bundle: &'a PromptBundle,
    head_start: Duration,
) -> ShardsRequest<'a> {
    ShardsRequest {
        packs,
        roles,
        intent: None,
        bundle,
        json_schema: SCHEMA,
        system_prompt_file: PathBuf::from("/base/plugin/prompts/v1/system.md"),
        add_dir: PathBuf::from("/work/pr"),
        timeout: Duration::from_secs(600),
        head_start,
    }
}

/// `count` packs, `PACK-1`, `PACK-2`, ...
fn packs(count: usize) -> Vec<String> {
    (1..=count).map(|n| format!("PACK-{n}")).collect()
}

/// Runs `roles` over `packs` through `fake` with the stand-in bundle: the bundle's failure is
/// the outer error, the run's result the inner one.
fn run(
    fake: &Fake,
    packs: &[String],
    roles: &[RequiredRole],
    head_start: Duration,
) -> Result<Result<ShardedSpecialists<Unreachable>, ShardsError>, Box<dyn Error>> {
    let bundle = bundle()?;
    Ok(run_shards(
        fake,
        &request(packs, roles, &bundle, head_start),
    ))
}

/// Every role of `FULL`, with whether it completed: `bugs`, `security`, `design` in order.
fn full(completed: [bool; 3]) -> Vec<(Role, bool)> {
    [Role::Bugs, Role::Security, Role::Design]
        .into_iter()
        .zip(completed)
        .collect()
}

/// Each role of a shard and whether it completed.
fn completed(shard: &Specialists<Unreachable>) -> Vec<(Role, bool)> {
    shard
        .roles
        .iter()
        .map(|report| {
            (
                report.role,
                matches!(report.outcome, RoleOutcome::Completed(_)),
            )
        })
        .collect()
}

/// The pack each call was made with, in the order the calls started.
fn packs_called(fake: &Fake) -> Vec<String> {
    fake.seen()
        .calls
        .iter()
        .filter_map(|call| {
            let (pack, _) = call.user_prompt.as_str().split_once("\n\n")?;
            Some(pack.to_owned())
        })
        .collect()
}

#[test]
fn each_shard_runs_every_role_over_its_own_pack() -> Result<(), Box<dyn Error>> {
    let fake = Fake::answering();
    let packs = packs(2);

    let sharded = run(&fake, &packs, &FULL, LONG)??;

    let shards: Vec<_> = sharded.shards.iter().map(completed).collect();
    assert_eq!(shards, [full([true; 3]), full([true; 3])]);
    let mut prompts: Vec<String> = fake
        .seen()
        .calls
        .iter()
        .map(|call| call.user_prompt.as_str().to_owned())
        .collect();
    prompts.sort_unstable();
    assert_eq!(
        prompts,
        [
            "PACK-1\n\nBUGS",
            "PACK-1\n\nDESIGN",
            "PACK-1\n\nSECURITY",
            "PACK-2\n\nBUGS",
            "PACK-2\n\nDESIGN",
            "PACK-2\n\nSECURITY",
        ]
    );
    Ok(())
}

#[test]
fn never_more_than_four_processes_run_across_three_shards() -> Result<(), Box<dyn Error>> {
    // Every call of a shard waits until four are in flight at once, so each shard's calls
    // overlap fully; a shard that started before the last one ended would show in the steps.
    let fake = Fake::new(
        [],
        Hold::UntilInFlight {
            in_flight: 4,
            total: 4,
        },
    );
    let packs = packs(3);

    let sharded = run(&fake, &packs, &FULL_META, Duration::ZERO)??;

    assert_eq!(sharded.shards.len(), 3);
    let seen = fake.seen();
    assert_eq!((seen.peak, seen.started), (4, 12));
    for steps in seen.steps.chunks(8) {
        let starts = steps
            .iter()
            .filter(|step| matches!(step, Step::Start(_)))
            .count();
        assert_eq!(starts, 4, "a shard's calls all end before the next starts");
    }
    drop(seen);
    let called = packs_called(&fake);
    let by_shard: Vec<BTreeSet<&str>> = called
        .chunks(4)
        .map(|chunk| chunk.iter().map(String::as_str).collect())
        .collect();
    assert_eq!(
        by_shard,
        [
            BTreeSet::from(["PACK-1"]),
            BTreeSet::from(["PACK-2"]),
            BTreeSet::from(["PACK-3"]),
        ]
    );
    Ok(())
}

#[test]
fn a_quota_exhausted_shard_skips_the_shards_after_it() -> Result<(), Box<dyn Error>> {
    let quota = Answer::Ran(ended(ModelOutcome::QuotaExhausted, Aeu::ZERO));
    let fake = Fake::new([(Role::Bugs, vec![quota])], Hold::Never);
    let packs = packs(3);

    let sharded = run(&fake, &packs, &FULL, LONG)??;

    let [first, second, third] = sharded.shards.as_slice() else {
        return Err("three shards".into());
    };
    assert_eq!(first.halt, Some(Halt::RateLimited));
    for shard in [second, third] {
        assert!(shard.roles.iter().all(|report| {
            report.outcome == RoleOutcome::Skipped(Halt::RateLimited) && report.attempts == 0
        }));
        assert_eq!(shard.halt, Some(Halt::RateLimited));
    }
    assert_eq!(packs_called(&fake), ["PACK-1"]);
    Ok(())
}

#[test]
fn a_quota_pause_in_a_retry_after_the_run_cap_halt_skips_the_later_shards()
-> Result<(), Box<dyn Error>> {
    // Bugs goes alone, answers malformed and leaves $0.40 of the shard's $1.60, so the run cap
    // halts the other roles; its retry gets the $0.40 and reports the quota exhausted.
    let malformed = Answer::Ran(ended(ModelOutcome::Malformed, Aeu::from_micros(1_200_000)));
    let quota = Answer::Ran(ended(ModelOutcome::QuotaExhausted, Aeu::ZERO));
    let fake = Fake::new([(Role::Bugs, vec![malformed, quota])], Hold::Never);
    let packs = packs(2);

    let sharded = run(&fake, &packs, &FULL, LONG)??;

    let [first, second] = sharded.shards.as_slice() else {
        return Err("two shards".into());
    };
    assert_eq!(first.halt, Some(Halt::RateLimited));
    let budgets: Vec<Aeu> = fake.calls_of(Role::Bugs).iter().map(|c| c.budget).collect();
    assert_eq!(budgets, [Role::Bugs.cap(), Aeu::from_micros(400_000)]);
    assert!(
        second
            .roles
            .iter()
            .all(|report| report.outcome == RoleOutcome::Skipped(Halt::RateLimited))
    );
    assert_eq!(packs_called(&fake), ["PACK-1", "PACK-1"]);
    Ok(())
}

#[test]
fn a_panicking_runner_skips_the_shards_after_it() -> Result<(), Box<dyn Error>> {
    let fake = Fake::new([(Role::Bugs, vec![Answer::Panics])], Hold::Never);
    let packs = packs(2);

    let sharded = run(&fake, &packs, &FULL, LONG)??;

    let [first, second] = sharded.shards.as_slice() else {
        return Err("two shards".into());
    };
    assert_eq!(
        first.roles.first().map(|report| &report.outcome),
        Some(&RoleOutcome::Failed(RoleFailure::Panicked))
    );
    assert!(
        second
            .roles
            .iter()
            .all(|report| report.outcome == RoleOutcome::Skipped(Halt::Panicked))
    );
    assert_eq!(packs_called(&fake), ["PACK-1"]);
    Ok(())
}

#[test]
fn a_shard_that_reaches_its_run_cap_does_not_stop_the_next() -> Result<(), Box<dyn Error>> {
    // Bugs goes alone and spends the whole shard's cap, so the others no longer fit.
    let whole_cap = Aeu::from_micros(1_600_000);
    let fake = Fake::new(
        [(
            Role::Bugs,
            vec![Answer::Ran(ran(document_from("bugs"), whole_cap))],
        )],
        Hold::Never,
    );
    let packs = packs(2);

    let sharded = run(&fake, &packs, &FULL, LONG)??;

    let shards: Vec<_> = sharded.shards.iter().map(completed).collect();
    assert_eq!(shards, [full([true, false, false]), full([true; 3])]);
    assert_eq!(
        sharded.shards.first().map(|shard| shard.halt),
        Some(Some(Halt::RunCap))
    );
    Ok(())
}

#[test]
fn an_invalid_pack_in_a_later_shard_is_refused_before_any_call() -> Result<(), Box<dyn Error>> {
    let fake = Fake::answering();
    let packs = ["PACK-1".to_owned(), "PACK\0".to_owned()];

    let refused = run(&fake, &packs, &FULL, LONG)?;

    assert!(matches!(
        refused,
        Err(ShardsError::Prompt { shard: 1, ref source }) if source.reason() == PromptError::Nul
    ));
    assert_eq!(fake.seen().started, 0);
    Ok(())
}

#[test]
fn no_pack_is_refused() -> Result<(), Box<dyn Error>> {
    let fake = Fake::answering();

    let refused = run(&fake, &[], &FULL, LONG)?;

    assert!(matches!(refused, Err(ShardsError::Count { packs: 0 })));
    assert_eq!(fake.seen().started, 0);
    Ok(())
}

#[test]
fn four_packs_are_refused() -> Result<(), Box<dyn Error>> {
    let fake = Fake::answering();
    let packs = packs(4);

    let refused = run(&fake, &packs, &FULL, LONG)?;

    assert!(matches!(refused, Err(ShardsError::Count { packs: 4 })));
    assert_eq!(fake.seen().started, 0);
    Ok(())
}

#[test]
fn a_role_completed_on_every_shard_counts_and_costs_sum() -> Result<(), Box<dyn Error>> {
    let security = Answer::Ran(ran(document_from("security"), tenth_of(Role::Security)));
    let fake = Fake::new(
        [(Role::Security, vec![security, Answer::Fails])],
        Hold::Never,
    );
    let packs = packs(2);

    let sharded = run(&fake, &packs, &FULL, LONG)??;

    assert_eq!(
        sharded.completed_roles(),
        BTreeSet::from([RequiredRole::Bugs, RequiredRole::Design])
    );
    let runner_failed = sharded
        .shards
        .last()
        .and_then(|shard| shard.roles.get(1))
        .map(|report| &report.outcome);
    assert_eq!(
        runner_failed,
        Some(&RoleOutcome::Failed(RoleFailure::Runner(Unreachable)))
    );
    // Five calls at a tenth of their caps, and the failed one charged its whole cap.
    let tenths = [
        Role::Bugs,
        Role::Design,
        Role::Bugs,
        Role::Security,
        Role::Design,
    ]
    .into_iter()
    .fold(Aeu::ZERO, |sum, role| sum.saturating_add(tenth_of(role)));
    assert_eq!(sharded.cost(), tenths.saturating_add(Role::Security.cap()));
    assert_eq!(sharded.run_cap(), Aeu::from_micros(3_200_000));
    Ok(())
}
