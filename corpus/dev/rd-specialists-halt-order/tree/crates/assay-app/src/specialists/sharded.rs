//! Sharded runs (04 §3, #98): the shards of a large pack run one after another, each through
//! the whole fan-out with every role over its own pack, so a run never has more than
//! [`MAX_MODEL_PROCESSES`](super::MAX_MODEL_PROCESSES) calls in flight.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use assay_domain::{Aeu, RequiredRole};

use super::{Halt, Prepared, Specialists, SpecialistsRequest};
use crate::model_runner::{InvalidPrompt, ModelRunner};
use crate::prompt_bundle::PromptBundle;
use crate::shard::{MAX_SHARDS, ShardCount};

/// What to run over each shard, and how to call the model: a [`SpecialistsRequest`] with one
/// context pack per shard.
#[derive(Debug)]
pub struct ShardsRequest<'a> {
    /// One context pack per shard, in the order the shards run: one to [`MAX_SHARDS`]. Each
    /// must not start with `-` (see [`Prompt`](crate::Prompt)).
    pub packs: &'a [String],
    /// The roles the plan requires; each shard runs every one, once, in roster order.
    pub roles: &'a [RequiredRole],
    /// The intent envelope, sent after the pack to the roles that see it, on every shard.
    pub intent: Option<&'a str>,
    /// The prompt bundle.
    pub bundle: &'a PromptBundle,
    /// The JSON Schema of the findings every role returns, as JSON text.
    pub json_schema: &'a str,
    /// The base-sourced system prompt file (absolute path).
    pub system_prompt_file: PathBuf,
    /// The read-only PR tree the roles may read (absolute path).
    pub add_dir: PathBuf,
    /// The wall-clock limit of each call.
    pub timeout: Duration,
    /// How long each shard's first call runs alone: the packs differ, so each shard writes its
    /// own prompt cache entry.
    pub head_start: Duration,
}

impl<'a> ShardsRequest<'a> {
    /// The fan-out request of the shard whose pack is `pack`.
    fn for_pack(&self, pack: &'a str) -> SpecialistsRequest<'a> {
        SpecialistsRequest {
            roles: self.roles,
            context_pack: pack,
            intent: self.intent,
            bundle: self.bundle,
            json_schema: self.json_schema,
            // Each shard's calls own their paths (`ModelCall` stores them); at most three.
            system_prompt_file: self.system_prompt_file.clone(),
            add_dir: self.add_dir.clone(),
            timeout: self.timeout,
            head_start: self.head_start,
        }
    }
}

/// Why [`run_shards`] made no call.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ShardsError {
    /// No pack, or more than [`MAX_SHARDS`].
    #[error("{packs} shard packs; a run has 1 to {MAX_SHARDS}")]
    Count {
        /// How many packs were given.
        packs: usize,
    },
    /// A shard's pack (or the intent) does not make a valid prompt.
    #[error("the prompts of shard {shard} are not valid")]
    Prompt {
        /// The shard, counted from 0.
        shard: usize,
        /// Why its prompt was refused.
        #[source]
        source: InvalidPrompt,
    },
}

/// The outcome of a sharded run: one fan-out per shard, in shard order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardedSpecialists<E> {
    /// Each shard's fan-out. A shard after one whose stage paused or stopped never starts:
    /// every role is [`Skipped`](super::RoleOutcome::Skipped) for that reason.
    pub shards: Vec<Specialists<E>>,
}

impl<E> ShardedSpecialists<E> {
    /// What every call of every shard cost, to ledger.
    #[must_use]
    pub fn cost(&self) -> Aeu {
        self.shards
            .iter()
            .fold(Aeu::ZERO, |total, shard| total.saturating_add(shard.cost()))
    }

    /// The roles that produced structured output on every shard, for `decide`'s
    /// `completed_roles` (04 §5): a role that did not complete on one shard did not review
    /// that shard's files.
    #[must_use]
    pub fn completed_roles(&self) -> BTreeSet<RequiredRole> {
        let mut shards = self.shards.iter();
        let Some(first) = shards.next() else {
            return BTreeSet::new();
        };
        shards.fold(first.completed_roles(), |mut roles, shard| {
            let completed = shard.completed_roles();
            roles.retain(|role| completed.contains(role));
            roles
        })
    }

    /// The specialists' run cap over all shards: each shard's, summed (06 §1).
    #[must_use]
    pub fn run_cap(&self) -> Aeu {
        self.shards.iter().fold(Aeu::ZERO, |total, shard| {
            total.saturating_add(shard.run_cap)
        })
    }
}

/// Runs every role over each pack of `request`, one shard after another.
///
/// Each shard runs as
/// [`run_specialists`](crate::run_specialists) runs it under its own run cap (the roles' caps),
/// so the calls in flight never exceed [`MAX_MODEL_PROCESSES`](super::MAX_MODEL_PROCESSES) and
/// the shards together stay within the scaled cap ([`full_run_cap`](crate::full_run_cap)).
///
/// A shard whose run cap skipped roles does not stop the next, which has its own cap. A shard
/// whose stage paused (quota, a `rejected` or `allowed_warning` rate limit) or stopped (a
/// runner's panic) starts no later shard: each is reported with every role skipped for that
/// reason (06 §1). Every thread is joined before this returns.
///
/// # Errors
/// Returns [`ShardsError`] before any call: [`Count`](ShardsError::Count) for no pack or more
/// than [`MAX_SHARDS`], [`Prompt`](ShardsError::Prompt) for the first shard whose prompts are
/// not valid (its pack starts with `-`, or the pack or intent holds a NUL byte).
pub fn run_shards<R>(
    runner: &R,
    request: &ShardsRequest<'_>,
) -> Result<ShardedSpecialists<R::Error>, ShardsError>
where
    R: ModelRunner + Sync,
    R::Error: Send,
{
    let packs = request.packs.len();
    if ShardCount::new(packs).is_none() {
        return Err(ShardsError::Count { packs });
    }
    let prepared = request
        .packs
        .iter()
        .enumerate()
        .map(|(shard, pack)| {
            Prepared::new(request.for_pack(pack))
                .map_err(|source| ShardsError::Prompt { shard, source })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut halted = None;
    let mut shards = Vec::with_capacity(prepared.len());
    for shard in prepared {
        let ran = match halted {
            Some(halt) => shard.skipped(halt),
            None => shard.run(runner),
        };
        if let Some(halt @ (Halt::RateLimited | Halt::Panicked)) = ran.halt {
            halted.get_or_insert(halt);
        }
        shards.push(ran);
    }
    Ok(ShardedSpecialists { shards })
}
