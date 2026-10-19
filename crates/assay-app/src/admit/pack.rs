//! The Z4 half of the context pack, `assay.context-pack/1` (04 §3 "Context pack", 02 §2, R3-02):
//! the base suppress text, the owner-valid dismissals from the ledger's command records and the
//! prior findings from the previous run's bundle, read through the ledger's evidence pointer.
//! Nothing in it comes from PR comment text. Z3 treats it as data and checks it by the digest
//! `admit` outputs.

use assay_domain::RunUid;
use serde::Serialize;

use super::dismissals::OwnerDismissal;
use crate::config::Sha256Digest;
use crate::rereview::CarriedFinding;
use crate::rereview::record::RecordOut;

/// The contract id the pack carries.
const PACK_SCHEMA: &str = "assay.context-pack/1";

/// The longest base suppress text the pack carries, in bytes; a longer one fails `admit`.
pub const MAX_SUPPRESS_BYTES: usize = 65_536;

/// The pack as `admit` wrote it, and its digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextPack {
    json: Vec<u8>,
    digest: Sha256Digest,
}

impl ContextPack {
    /// The pack's JSON, one `assay.context-pack/1` document.
    #[must_use]
    pub fn json(&self) -> &[u8] {
        &self.json
    }

    /// The sha256 of [`Self::json`], published in `admit`'s outputs so the review checks the
    /// artifact it downloads (02 §1).
    #[must_use]
    pub const fn digest(&self) -> &Sha256Digest {
        &self.digest
    }
}

/// What the pack is written from.
#[derive(Debug, Clone, Copy)]
pub(super) struct PackParts<'a> {
    /// This run.
    pub(super) run_uid: RunUid,
    /// Its evidence key.
    pub(super) run_key: &'a Sha256Digest,
    /// The base `.assay/suppress.toml` text, as read at the pinned base.
    pub(super) suppress: Option<&'a str>,
    /// The owner-valid dismissals.
    pub(super) dismissals: &'a [OwnerDismissal],
    /// The previous run's open findings.
    pub(super) prior: &'a [CarriedFinding],
}

/// The pack's wire form (OUT-01).
#[derive(Serialize)]
struct PackWire<'a> {
    schema: &'static str,
    run_uid: RunUid,
    run_key: &'a str,
    suppress: Option<&'a str>,
    dismissals: &'a [OwnerDismissal],
    prior: Vec<RecordOut<'a>>,
}

/// The pack of `parts`, with its digest.
pub(super) fn pack(parts: PackParts<'_>) -> Result<ContextPack, serde_json::Error> {
    let wire = PackWire {
        schema: PACK_SCHEMA,
        run_uid: parts.run_uid,
        run_key: parts.run_key.as_str(),
        suppress: parts.suppress,
        dismissals: parts.dismissals,
        prior: parts.prior.iter().map(RecordOut::of).collect(),
    };
    let json = serde_json::to_vec(&wire)?;
    let digest = Sha256Digest::of(&json);
    Ok(ContextPack { json, digest })
}
