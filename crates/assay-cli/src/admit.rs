//! The pipeline's `admit` job (#60 ST-11, S1a, 04 §1): plan's `run.json`, authenticated by
//! plan's `run_digest`, admitted through [`assay_app::admit`], with the admitted `run.json` and,
//! for a spend, `context-pack.json` written into `--out` and admit's outputs (03 §4a) read off
//! what was written.
//!
//! A run with no LLM stage reads nothing ([`assay_app::plan_stage`]). Otherwise, in order: the
//! pinned base and head are fetched; the base P10 lists and `.assay/suppress.toml` are read at
//! the base; the range's identities through [`assay_app::range_identities`], decide's one range
//! reading; the previous completed run's head from its bundle through the ledger's evidence
//! pointer (D6 A, [`assay_app::prior_head`]) and with it the carry through
//! [`assay_app::review_target`]. The binary builds the ports; [`run_admit`] does the rest over
//! them, so it is tested with fakes.

mod error;

use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;

use assay_app::{
    AdmitPorts, AdmitRequest, Admitted, BundleSource, Carry, Clock, CommandComments, CommitId,
    ContractValidator, EntryKind, Forge, Ledger, LedgerRefs, MAX_SUPPRESS_BYTES, NoEvents,
    PlanStage, PriorCarry, PullRequestObjects, RangeRequest, Sha256Digest, TargetRequest, admit,
    base_identities, plan_stage, prior_head, range_identities, read_pushes, read_state,
    review_target,
};
use assay_forge::git::GitError;
use assay_forge::process::ProcessError;

pub use self::error::{AdmitRunError, AdmitRunFailure, FetchedSide, SuppressError};
use crate::ci::{FirstAttempt, OutDir};
use crate::outputs::{AdmitOutputs, Identity, PlanOutputs};
use crate::plan::RUN_FILE;
use crate::pull_request::{ForgeObjectsError, ForgePullRequest};

/// The context pack's file name in `--out`, beside [`RUN_FILE`]; written for a spend only.
pub const PACK_FILE: &str = "context-pack.json";

/// The base suppress rules, carried in the pack as they are (03 §1).
const SUPPRESS: &str = ".assay/suppress.toml";

/// Fetches a commit, with its history, into the object repository the job reads.
pub trait FetchCommits {
    /// Why the commit could not be fetched.
    type Error: std::error::Error + 'static;

    /// Fetches `commit`.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the commit could not be fetched.
    fn fetch(&self, commit: &CommitId) -> Result<(), Self::Error>;
}

/// What `admit` is given besides its ports.
#[derive(Debug, Clone, Copy)]
pub struct AdmitJob<'a> {
    /// The run, checked to be its first attempt.
    pub run: &'a FirstAttempt,
    /// guard's identity of the PR (I2).
    pub identity: &'a Identity,
    /// plan's outputs.
    pub plan: &'a PlanOutputs,
    /// The bytes of the `run.json` in plan's artifact, not yet authenticated.
    pub document: &'a [u8],
    /// The contract validator the documents are checked with.
    pub validator: &'a ContractValidator,
}

/// What `admit` reads and writes through, borrowed for one call.
#[derive(Debug)]
pub struct AdmitJobPorts<'p, 'r, L, R, B, C, F, H, K> {
    /// Fetches the pinned commits into [`Self::objects`]' repository.
    pub fetch: &'p H,
    /// The PR's objects.
    pub objects: &'p ForgePullRequest<'r>,
    /// GitHub, for the range's commit identities.
    pub forge: &'p F,
    /// The ledger's files.
    pub ledger: &'p L,
    /// The ledger's refs.
    pub refs: &'p R,
    /// The run artifacts, for the previous run's bundle.
    pub bundles: &'p B,
    /// The PR's issue comments, to void a command record.
    pub comments: &'p C,
    /// The wall clock, and the slot wait.
    pub clock: &'p mut K,
}

/// Admits `job`'s run through `ports` and writes the admitted documents into `out`.
///
/// It creates `out` and writes the admitted `run.json` into it as [`RUN_FILE`] (the plan's
/// unchanged for a run that does not spend) and, for a spend, the context pack as
/// [`PACK_FILE`], and returns admit's outputs for what it wrote.
///
/// Before anything else the plan document's sha256 must be plan's `run_digest`; a run with no
/// LLM stage then reads no port.
///
/// # Errors
///
/// [`AdmitRunError`]: [`AdmitRunError::PlanDigest`] before anything is read, and every other
/// failure before or inside the admission. A failure to write `out` once a slot is held is
/// [`AdmitRunError::Write`] with the slot, so the job can still report it for release.
pub fn run_admit<L, R, B, C, F, H, K>(
    job: AdmitJob<'_>,
    mut ports: AdmitJobPorts<'_, '_, L, R, B, C, F, H, K>,
    out: &OutDir,
) -> Result<AdmitOutputs, AdmitRunFailure<L, R, B, C, F, H>>
where
    L: Ledger,
    R: LedgerRefs,
    B: BundleSource,
    C: CommandComments,
    F: Forge,
    H: FetchCommits,
    K: Clock,
{
    if Sha256Digest::of(job.document) != *job.plan.run_digest() {
        return Err(AdmitRunError::PlanDigest);
    }
    let run_uid = job.run.run_env().run_uid;
    let admitted =
        match plan_stage(job.validator, job.document, run_uid).map_err(AdmitRunError::Plan)? {
            PlanStage::NoSpend(admitted) => *admitted,
            PlanStage::Spends => spend(job, &mut ports)?,
        };
    let outputs = AdmitOutputs::new(&admitted, job.plan.tier());
    write_admitted(out, &admitted).map_err(|(path, source)| AdmitRunError::Write {
        path,
        source,
        held: outputs.held(),
    })?;
    Ok(outputs)
}

/// Reads what the admission needs and admits a run whose LLM stage would spend.
fn spend<L, R, B, C, F, H, K>(
    job: AdmitJob<'_>,
    ports: &mut AdmitJobPorts<'_, '_, L, R, B, C, F, H, K>,
) -> Result<Admitted, AdmitRunFailure<L, R, B, C, F, H>>
where
    L: Ledger,
    R: LedgerRefs,
    B: BundleSource,
    C: CommandComments,
    F: Forge,
    H: FetchCommits,
    K: Clock,
{
    let identity = job.identity;
    for (side, commit) in [
        (FetchedSide::Base, &identity.base_sha),
        (FetchedSide::Head, &identity.head_sha),
    ] {
        ports
            .fetch
            .fetch(commit)
            .map_err(|source| AdmitRunError::Fetch { side, source })?;
    }
    let objects = ports.objects;
    let identities =
        base_identities(objects, &identity.base_sha).map_err(AdmitRunError::Identities)?;
    let suppress = base_suppress(objects, &identity.base_sha).map_err(AdmitRunError::Suppress)?;
    let run_uid = job.run.run_env().run_uid;
    let pushes =
        read_pushes(ports.ledger, run_uid.repo_id, identity.pr).map_err(AdmitRunError::Pushes)?;
    let range = range_identities(
        objects,
        ports.forge,
        &RangeRequest {
            base_sha: &identity.base_sha,
            head_sha: &identity.head_sha,
            pushes: &pushes,
        },
    )
    .map_err(AdmitRunError::Range)?;
    let carry = carry::<L, R, B, C, F, H>(job, objects, ports.ledger, ports.bundles)?;
    let request = AdmitRequest {
        run_uid,
        plan: job.document,
        carry,
        identities: &identities,
        author: identity.author,
        range: &range,
        suppress: suppress.as_deref(),
    };
    let ports = AdmitPorts {
        ledger: ports.ledger,
        refs: ports.refs,
        bundles: ports.bundles,
        comments: ports.comments,
        clock: &mut *ports.clock,
        // D7 is open: admit's events are not part of its artifact yet.
        events: &mut NoEvents,
    };
    admit(ports, &request, job.validator).map_err(|error| AdmitRunError::Admit(Box::new(error)))
}

/// Whether the previous completed run's open findings carry (04 §7): its head is read from its
/// bundle through the ledger's evidence pointer (D6 A), and [`review_target`] decides.
///
/// The previous head is not fetched: when it is an ancestor of head, head's fetch brought it,
/// and otherwise the carry resets whether it is rewritten or unreachable.
fn carry<L, R, B, C, F, H>(
    job: AdmitJob<'_>,
    objects: &ForgePullRequest<'_>,
    ledger: &L,
    bundles: &B,
) -> Result<Carry, AdmitRunFailure<L, R, B, C, F, H>>
where
    L: Ledger,
    R: LedgerRefs,
    B: BundleSource,
    C: CommandComments,
    F: Forge,
    H: FetchCommits,
{
    let (run, identity) = (job.run.run_env(), job.identity);
    let state =
        read_state(ledger, run.run_uid.repo_id, identity.pr).map_err(AdmitRunError::State)?;
    let previous = match &state.evidence {
        None => None,
        Some(pointer) => {
            let repository = run.repo.to_string();
            let prior = PriorCarry {
                repo_id: run.run_uid.repo_id,
                pointer,
                repository: &repository,
            };
            Some(prior_head(&prior, bundles).map_err(AdmitRunError::PriorHead)?)
        }
    };
    let request = TargetRequest {
        base: &identity.base_sha,
        head: &identity.head_sha,
        previous: previous.as_ref(),
    };
    let target = review_target(&request, objects).map_err(AdmitRunError::Target)?;
    if target.range_diff_failure().is_some_and(group_may_run) {
        return Err(AdmitRunError::RangeDiffKilled);
    }
    Ok(target.carry())
}

/// Whether `error` leaves a git process group that may still be running: the job must stop.
const fn group_may_run(error: &ForgeObjectsError) -> bool {
    matches!(
        error,
        ForgeObjectsError::Git(GitError::Process {
            source: ProcessError::Kill { .. },
            ..
        })
    )
}

/// The text of base `.assay/suppress.toml` at `base`, or `None` when the base has none.
fn base_suppress(
    objects: &ForgePullRequest<'_>,
    base: &CommitId,
) -> Result<Option<String>, SuppressError> {
    let entries = objects
        .list(base, Some(SUPPRESS))
        .map_err(SuppressError::Objects)?;
    let entry = match entries.as_slice() {
        [] => return Ok(None),
        [entry] if entry.path == SUPPRESS && entry.kind == EntryKind::File => entry,
        // A directory lists what is under it; a symlink or submodule is not followed.
        _ => return Err(SuppressError::NotARegularFile),
    };
    let size = entry.size.ok_or(SuppressError::NotARegularFile)?;
    if usize::try_from(size).map_or(true, |size| size > MAX_SUPPRESS_BYTES) {
        return Err(SuppressError::TooLarge { size });
    }
    let blobs = objects.read(&[entry]).map_err(SuppressError::Objects)?;
    let [bytes] = <[Vec<u8>; 1]>::try_from(blobs).map_err(|_not_one| SuppressError::Unread)?;
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_not_utf8| SuppressError::NotUtf8)
}

/// Creates `out` (refusing one that exists by now) and writes the admitted documents into it.
fn write_admitted(out: &OutDir, admitted: &Admitted) -> Result<(), (PathBuf, std::io::Error)> {
    fs::create_dir(out.path()).map_err(|source| (out.path().to_path_buf(), source))?;
    let pack = admitted.pack().map(|pack| (PACK_FILE, pack.json()));
    for (name, bytes) in [(RUN_FILE, admitted.run_json())].into_iter().chain(pack) {
        let path = out.path().join(name);
        if let Err(source) = File::create_new(&path)
            .and_then(|mut file| file.write_all(bytes).and_then(|()| file.flush()))
        {
            return Err((path, source));
        }
    }
    Ok(())
}
