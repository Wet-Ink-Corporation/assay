//! The owner-valid dismissals of the context pack (04 §6, 05 T21, R3-02, R3-13): read from the
//! ledger's command records only, never from comment text. A record counts when its command is
//! exactly `/assay dismiss <fp> <reason>`, its actor is eligible under 04 §5 (a privileged
//! identity who is no agent, not the PR's author, no author or committer of its range and not one
//! of its pushers), and its comment still stands as recorded: created when recorded, not deleted,
//! never edited, its body hashing to the record's `body_sha256`.

use std::collections::BTreeSet;
use std::num::NonZeroU64;

use assay_domain::Fingerprint;
use serde::Serialize;

use super::comments::CommandComments;
use super::range::RangeIdentities;
use crate::config::Sha256Digest;
use crate::ledger::CommandRecord;
use crate::status::{Identities, UserId};

/// The most dismissals a pack carries; past it records are left out, which publishes more,
/// never less (fail closed). It also bounds the comment reads.
pub const MAX_DISMISSALS: usize = 256;

/// The command's tokens before the fingerprint.
const DISMISS: &str = "/assay dismiss ";

/// A dismissal as the pack carries it: the record's words, never the comment's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct OwnerDismissal {
    comment_id: NonZeroU64,
    actor_id: NonZeroU64,
    fp: Fingerprint,
    reason: String,
}

/// Who may dismiss on this PR.
#[derive(Debug, Clone, Copy)]
pub(super) struct Eligible<'a> {
    /// The base P10 lists.
    pub(super) identities: &'a Identities,
    /// The PR's author.
    pub(super) author: UserId,
    /// The authors and committers of the PR's range.
    pub(super) range: &'a RangeIdentities,
    /// Everyone who pushed to the PR, from its push records.
    pub(super) pushers: &'a BTreeSet<UserId>,
}

impl Eligible<'_> {
    /// Whether `actor`'s dismissal suppresses: the eligibility rule of 04 §5 (a self-dismiss is
    /// feedback only, 04 §6). A privileged identity that is no agent, not (a) the author, not (b)
    /// an author or committer of the range, and not (c) a pusher. A range commit attributed to no
    /// GitHub user counts as authored by every owner; the base lists name owners and approvers
    /// together, so then no identity is eligible (fail closed).
    fn admits(&self, actor: UserId) -> bool {
        self.identities.privileged.contains(&actor)
            && !self.identities.agents.contains(&actor)
            && actor != self.author
            && !self.range.made(actor)
            && !self.range.has_unattributed()
            && !self.pushers.contains(&actor)
    }
}

/// The dismissals among `records` that stand, in record order, at most [`MAX_DISMISSALS`].
///
/// # Errors
///
/// The port's error when a candidate's comment cannot be read.
pub(super) fn valid_dismissals<C: CommandComments>(
    records: Vec<CommandRecord>,
    eligible: Eligible<'_>,
    comments: &C,
) -> Result<Vec<OwnerDismissal>, C::Error> {
    let mut kept = Vec::new();
    let candidates = records
        .into_iter()
        .filter(|record| eligible.admits(record.actor_id))
        .filter_map(|record| {
            let (fp, reason) = parse_dismiss(&record.command)?;
            let dismissal = OwnerDismissal {
                comment_id: record.comment_id.0,
                actor_id: record.actor_id.0,
                fp,
                reason: reason.to_owned(),
            };
            Some((record, dismissal))
        })
        .take(MAX_DISMISSALS);
    for (record, dismissal) in candidates {
        if stands(&record, comments)? {
            kept.push(dismissal);
        }
    }
    Ok(kept)
}

/// The fingerprint and reason of `command` when it is exactly `/assay dismiss <fp> <reason>`:
/// single spaces, a fingerprint of 16 lowercase hex digits and a reason that starts with a
/// character other than a space.
fn parse_dismiss(command: &str) -> Option<(Fingerprint, &str)> {
    let (fp, reason) = command.strip_prefix(DISMISS)?.split_once(' ')?;
    let Ok(fp) = fp.parse::<Fingerprint>() else {
        return None;
    };
    let starts = reason.chars().next()?;
    (!starts.is_whitespace()).then_some((fp, reason))
}

/// Whether `record`'s comment still stands as recorded (05 T21): present, created when the
/// record says, never edited, and with the body whose hash the record holds.
fn stands<C: CommandComments>(record: &CommandRecord, comments: &C) -> Result<bool, C::Error> {
    Ok(comments.comment(record.comment_id)?.is_some_and(|now| {
        now.created_at == record.created_at
            && now.updated_at == now.created_at
            && Sha256Digest::of(now.body.as_bytes()) == record.body_sha256
    }))
}

#[cfg(test)]
mod tests {
    use super::parse_dismiss;

    #[test]
    fn reads_the_fingerprint_and_reason_of_an_exact_dismiss() {
        let parsed = parse_dismiss("/assay dismiss 0123456789abcdef not a bug here");

        let fp = "0123456789abcdef".parse().expect("16 hex digits");
        assert_eq!(parsed, Some((fp, "not a bug here")));
    }

    #[test]
    fn refuses_any_other_shape() {
        let refused = [
            "/assay recheck",
            "/assay dismiss 0123456789abcdef",
            "/assay dismiss 0123456789abcdef ",
            "/assay dismiss 0123456789abcdef  two spaces",
            "/assay dismiss 0123456789ABCDEF upper case",
            "/assay dismiss 0123456789abcde short",
            "/assay  dismiss 0123456789abcdef doubled",
            " /assay dismiss 0123456789abcdef leading",
        ]
        .map(parse_dismiss);

        assert_eq!(refused, [None; 8]);
    }
}
