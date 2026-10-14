//! The digests a decision carries: `inputs_digest` over what `decide` read, and the reasons
//! digest an override quotes (`docs/architecture/04-pipeline.md` §5).
//!
//! `inputs_digest` hashes a canonical encoding defined here rather than any serialisation
//! format: every value is written in field order, strings and lists with a length prefix,
//! enums by their wire code. Each `Canonical` impl destructures its type exhaustively, so a new
//! field does not compile until it is part of the digest.

use std::collections::BTreeSet;
use std::fmt;

use sha2::{Digest, Sha256};

use crate::decide::inputs::{
    Authorship, CallFailure, DecisionInputs, Deferral, GateFinding, GateOutcome, GateReport,
    InfraPolicy, LlmPolicy, LlmStage, NotRunReason, Override, OverrideScope, OwnerApproval, Policy,
    PullRequest, Readiness, ReviewedFinding, StepAttribution, SupportOutcome, Waiver, WeighIn,
};
use crate::decision::{DegradedRole, FindingDisposition, Reason};
use crate::disposition::Disposition;
use crate::event::VerifierVerdict;
use crate::findings::GateConclusion;
use crate::fingerprint::Fingerprint;
use crate::reason::ReasonCode;
use crate::rubric::Rubric;
use crate::severity::Severity;
use crate::tier::{PrClass, Tier};
use crate::tiering::RequiredRole;

/// Separates this encoding from any other use of SHA-256 over similar bytes.
const INPUTS_DOMAIN: &str = "assay.decide-inputs/1";

/// Bytes displayed as lowercase hex.
struct Hex<'a>(&'a [u8]);

impl fmt::Display for Hex<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

/// `sha256:<lowercase hex>` of the bytes fed into `hasher`.
fn render(hasher: Sha256) -> String {
    format!("sha256:{}", Hex(&hasher.finalize()))
}

/// The reasons digest of 04 §5: `sha256:<hex>` of the reason codes sorted by wire code,
/// duplicates kept, each followed by a newline; `override` reasons are left out, so the
/// digest of an overridden decision is the one its override quoted. Reproducible by hand with
/// `printf 'gate-failed\nneeds-split\n' | sha256sum`.
pub(crate) fn reasons_digest<'a>(reasons: impl IntoIterator<Item = &'a Reason>) -> String {
    let mut codes: Vec<&str> = reasons
        .into_iter()
        .filter(|reason| reason.code != ReasonCode::Override)
        .map(|reason| reason.code.code())
        .collect();
    codes.sort_unstable();
    let mut hasher = Sha256::new();
    for code in codes {
        hasher.update(code.as_bytes());
        hasher.update(b"\n");
    }
    render(hasher)
}

/// `sha256:<hex>` of the canonical encoding of the rubric and the inputs.
pub(crate) fn inputs_digest(rubric: &Rubric, inputs: &DecisionInputs) -> String {
    let mut encoder = Encoder(Sha256::new());
    INPUTS_DOMAIN.encode(&mut encoder);
    rubric.encode(&mut encoder);
    inputs.encode(&mut encoder);
    render(encoder.0)
}

/// Feeds a canonical encoding into SHA-256.
pub(crate) struct Encoder(Sha256);

impl Encoder {
    fn length(&mut self, length: usize) {
        // usize is at most 64 bits on every target Rust supports, so this never saturates.
        let length = u64::try_from(length).unwrap_or(u64::MAX);
        self.0.update(length.to_be_bytes());
    }
}

/// A value with a canonical encoding for `inputs_digest`.
pub(crate) trait Canonical {
    /// Writes this value's canonical encoding.
    fn encode(&self, encoder: &mut Encoder);
}

impl Canonical for str {
    fn encode(&self, encoder: &mut Encoder) {
        encoder.length(self.len());
        encoder.0.update(self.as_bytes());
    }
}

impl Canonical for String {
    fn encode(&self, encoder: &mut Encoder) {
        self.as_str().encode(encoder);
    }
}

/// A fingerprint is encoded as its wire text, 16 lowercase hex digits, length-prefixed like any
/// string, so the digest reads the same bytes the decision's JSON carries.
impl Canonical for Fingerprint {
    fn encode(&self, encoder: &mut Encoder) {
        self.to_string().encode(encoder);
    }
}

impl Canonical for u32 {
    fn encode(&self, encoder: &mut Encoder) {
        encoder.0.update(self.to_be_bytes());
    }
}

impl Canonical for bool {
    fn encode(&self, encoder: &mut Encoder) {
        encoder.0.update([u8::from(*self)]);
    }
}

impl<T: Canonical> Canonical for Option<T> {
    fn encode(&self, encoder: &mut Encoder) {
        self.is_some().encode(encoder);
        if let Some(value) = self {
            value.encode(encoder);
        }
    }
}

impl<T: Canonical> Canonical for [T] {
    fn encode(&self, encoder: &mut Encoder) {
        encoder.length(self.len());
        for item in self {
            item.encode(encoder);
        }
    }
}

impl<T: Canonical> Canonical for Vec<T> {
    fn encode(&self, encoder: &mut Encoder) {
        self.as_slice().encode(encoder);
    }
}

/// A set encodes like a list of its items in their order, so order and repetition in the
/// serialised inputs cannot change the digest.
impl<T: Canonical> Canonical for BTreeSet<T> {
    fn encode(&self, encoder: &mut Encoder) {
        encoder.length(self.len());
        for item in self {
            item.encode(encoder);
        }
    }
}

/// Closed vocabularies encode as their wire code.
macro_rules! canonical_by_code {
    ($($name:ty),+ $(,)?) => {
        $(
            impl Canonical for $name {
                fn encode(&self, encoder: &mut Encoder) {
                    self.code().encode(encoder);
                }
            }
        )+
    };
}

canonical_by_code!(
    Authorship,
    CallFailure,
    Deferral,
    Disposition,
    GateConclusion,
    InfraPolicy,
    LlmPolicy,
    NotRunReason,
    OverrideScope,
    OwnerApproval,
    PrClass,
    Readiness,
    RequiredRole,
    Severity,
    Tier,
    VerifierVerdict,
    WeighIn,
);

/// Encodes each field in turn, after exhaustively destructuring the struct.
macro_rules! canonical_struct {
    ($name:ident { $($field:ident),+ $(,)? }) => {
        impl Canonical for $name {
            fn encode(&self, encoder: &mut Encoder) {
                let Self { $($field),+ } = self;
                $($field.encode(encoder);)+
            }
        }
    };
}

canonical_struct!(DecisionInputs {
    decision_key,
    evidence_key,
    policy,
    pr,
    gates,
    weigh_in,
    llm,
    findings,
    dispositions,
    completed_roles,
    coordinator,
    verifier,
    degraded,
    waivers,
    override_record,
});
canonical_struct!(Policy {
    protected_bases,
    llm_blockers,
    llm_majors,
    on_infra_failure,
    block_on_infra_for,
});
canonical_struct!(PullRequest {
    base_ref,
    open_prs_on_head,
    tier,
    classes,
    owner_approval,
    authorship,
    readiness,
    requeues,
});
canonical_struct!(GateReport {
    name,
    required,
    outcome,
    findings
});
canonical_struct!(GateFinding { fp, rule, severity });
canonical_struct!(ReviewedFinding {
    fp,
    group_id,
    role,
    severity,
    pre_existing,
    verdict,
});
canonical_struct!(FindingDisposition { fp, code });
canonical_struct!(DegradedRole { role, reason });
canonical_struct!(Waiver { fp, by });
canonical_struct!(Override {
    scope,
    reasons_digest,
    by
});

impl Canonical for GateOutcome {
    fn encode(&self, encoder: &mut Encoder) {
        match self {
            Self::Ran { conclusion, steps } => {
                "ran".encode(encoder);
                conclusion.encode(encoder);
                steps.encode(encoder);
            }
            Self::Error { detail } => {
                "error".encode(encoder);
                detail.encode(encoder);
            }
            Self::Skipped => "skipped".encode(encoder),
        }
    }
}

impl Canonical for StepAttribution {
    fn encode(&self, encoder: &mut Encoder) {
        match self {
            Self::Unknown => "unknown".encode(encoder),
            Self::Known(steps) => {
                "known".encode(encoder);
                steps.encode(encoder);
            }
        }
    }
}

impl Canonical for SupportOutcome {
    fn encode(&self, encoder: &mut Encoder) {
        match self {
            Self::NotNeeded => "not-needed".encode(encoder),
            Self::Completed => "completed".encode(encoder),
            Self::Failed { reason } => {
                "failed".encode(encoder);
                reason.encode(encoder);
            }
        }
    }
}

impl Canonical for LlmStage {
    fn encode(&self, encoder: &mut Encoder) {
        match self {
            Self::NotRun { reason } => {
                "not-run".encode(encoder);
                reason.encode(encoder);
            }
            Self::Completed => "completed".encode(encoder),
            Self::Deferred { reason } => {
                "deferred".encode(encoder);
                reason.encode(encoder);
            }
        }
    }
}
