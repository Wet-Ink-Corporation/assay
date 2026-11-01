//! What `assay decide` hands `assay publish` (D5 A): the run's verdict state, written as
//! [`STATE_FILE`] beside the bundle in decide's `--out`, and read back by publish from its
//! `--state`. Both steps run in the same Z4 job and nothing PR-controlled writes there, so the
//! file is a trusted channel like the job outputs (03 §4a): unversioned, one closed shape per
//! state, read as strictly as a contract.
//!
//! `{"verdict":"silent"}`, `{"verdict":"refused","reason":{"code","detail"}}` or
//! `{"verdict":"decided","bundle_digest":"sha256:<hex>"}`, as compact JSON with sorted keys.

use std::error::Error;
use std::fmt;

use assay_app::{NotARefusal, RefusalReason, Sha256Digest, parse_json_strict};
use assay_domain::{Reason, ReasonCode};
use serde_json::{Map, Value, json};

/// The state's file name in decide's `--out`.
pub const STATE_FILE: &str = "state.json";

/// The largest state file read, in bytes (H-13): a refusal's detail is at most
/// [`assay_app::MAX_REFUSAL_DETAIL_BYTES`].
pub const MAX_STATE_BYTES: usize = 4096;

/// What `decide` concluded, for `publish`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecideState {
    /// The run was superseded, or yields to a newer one: publish nothing (R3-06).
    Silent,
    /// The run was refused: publish `failure` with this reason, and persist nothing.
    Refused(RefusalReason),
    /// The run was decided: publish the bundle `decide` wrote, whose sha256 this is.
    Decided {
        /// The sha256 of the bundle's exact bytes.
        digest: Sha256Digest,
    },
}

/// Why a state file was refused.
#[derive(Debug)]
pub enum DecideStateError {
    /// It is larger than [`MAX_STATE_BYTES`]; it was not parsed.
    TooLarge,
    /// It is not one JSON value, or an object in it repeats a key.
    Parse,
    /// It is JSON, but not exactly one of the states.
    Shape,
    /// Its reason is not one a refusal gives.
    Reason(NotARefusal),
}

impl fmt::Display for DecideStateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TooLarge => "decide's state is over its size cap",
            Self::Parse => "decide's state is not JSON without repeated keys",
            Self::Shape => "decide's state is not silent, refused or decided as decide writes it",
            Self::Reason(_) => "decide's state holds a reason no refusal gives",
        })
    }
}

impl Error for DecideStateError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Reason(source) => Some(source),
            Self::TooLarge | Self::Parse | Self::Shape => None,
        }
    }
}

impl DecideState {
    /// The state as [`STATE_FILE`] holds it.
    #[must_use]
    pub fn json(&self) -> Vec<u8> {
        let value = match self {
            Self::Silent => json!({ "verdict": "silent" }),
            Self::Refused(refusal) => {
                let reason = refusal.reason();
                json!({
                    "verdict": "refused",
                    "reason": { "code": reason.code.code(), "detail": reason.detail },
                })
            }
            Self::Decided { digest } => json!({
                "verdict": "decided",
                "bundle_digest": digest.to_string(),
            }),
        };
        value.to_string().into_bytes()
    }

    /// The state `bytes` hold.
    ///
    /// # Errors
    ///
    /// [`DecideStateError::TooLarge`] over [`MAX_STATE_BYTES`], unparsed; [`DecideStateError::Parse`] for
    /// anything but JSON without repeated keys; [`DecideStateError::Shape`] for anything but exactly
    /// one state's keys with values of their kind; [`DecideStateError::Reason`] for a reason no
    /// refusal gives.
    pub fn read(bytes: &[u8]) -> Result<Self, DecideStateError> {
        if bytes.len() > MAX_STATE_BYTES {
            return Err(DecideStateError::TooLarge);
        }
        let Value::Object(mut fields) =
            parse_json_strict(bytes).map_err(|_not_json| DecideStateError::Parse)?
        else {
            return Err(DecideStateError::Shape);
        };
        let state = match take_string(&mut fields, "verdict")?.as_str() {
            "silent" => Self::Silent,
            "refused" => {
                let Some(Value::Object(mut reason)) = fields.remove("reason") else {
                    return Err(DecideStateError::Shape);
                };
                let code = take_string(&mut reason, "code")?;
                let code = ReasonCode::ALL
                    .into_iter()
                    .find(|known| known.code() == code)
                    .ok_or(DecideStateError::Shape)?;
                let detail = take_string(&mut reason, "detail")?;
                if !reason.is_empty() {
                    return Err(DecideStateError::Shape);
                }
                let reason = Reason {
                    code,
                    detail,
                    fp: None,
                };
                Self::Refused(RefusalReason::try_from(reason).map_err(DecideStateError::Reason)?)
            }
            "decided" => Self::Decided {
                digest: take_string(&mut fields, "bundle_digest")?
                    .parse()
                    .map_err(|_not_a_digest| DecideStateError::Shape)?,
            },
            _ => return Err(DecideStateError::Shape),
        };
        if fields.is_empty() {
            Ok(state)
        } else {
            Err(DecideStateError::Shape)
        }
    }
}

/// Field `key` of `fields`, taken out; it must be a string.
fn take_string(fields: &mut Map<String, Value>, key: &str) -> Result<String, DecideStateError> {
    match fields.remove(key) {
        Some(Value::String(text)) => Ok(text),
        _ => Err(DecideStateError::Shape),
    }
}

#[cfg(test)]
mod tests;
