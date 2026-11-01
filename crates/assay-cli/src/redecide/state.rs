//! What `decide-only decide` hands `decide-only publish` (D11, J9): the re-decision's state,
//! written as [`STATE_FILE`](crate::STATE_FILE) beside the decision bundle in `--out`. Like the pipeline's
//! [`DecideState`](crate::DecideState), a trusted channel inside one job, unversioned and read
//! strictly; unlike it, a refusal is one of the two closed verdicts on unusable evidence, which
//! no pipeline refusal gives (`bundle-expired` is not a [`RefusalReason`](assay_app::RefusalReason)).
//!
//! `{"verdict":"silent"}`, `{"verdict":"refused","reason":{"code","detail"}}` with
//! `gate-error`/`bundle-digest` or `bundle-expired`/`missing|expired`, or
//! `{"verdict":"decided","bundle_digest":"sha256:<hex>"}`, as compact JSON with sorted keys.

use std::error::Error;
use std::fmt;

use assay_app::{Absence, Sha256Digest, parse_json_strict};
use assay_domain::{Reason, ReasonCode};
use serde_json::{Map, Value, json};

use crate::decide::MAX_STATE_BYTES;

/// Why the evidence bundle cannot be re-decided from: the refusal published for the head.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Unusable {
    /// The pointer or the bundle is not what assay wrote (F19): `gate-error:bundle-digest`.
    Forged,
    /// The bundle is gone (F20): `bundle-expired`, with how.
    Absent(Absence),
}

impl Unusable {
    /// Every verdict, in a fixed order.
    const ALL: [Self; 3] = [
        Self::Forged,
        Self::Absent(Absence::Missing),
        Self::Absent(Absence::Expired),
    ];

    /// The reason code and detail the refusal is published with.
    #[must_use]
    pub const fn code_and_detail(self) -> (ReasonCode, &'static str) {
        match self {
            Self::Forged => (ReasonCode::GateError, "bundle-digest"),
            Self::Absent(Absence::Missing) => (ReasonCode::BundleExpired, "missing"),
            Self::Absent(Absence::Expired) => (ReasonCode::BundleExpired, "expired"),
        }
    }

    /// The reason the refusal is published with.
    #[must_use]
    pub fn reason(self) -> Reason {
        let (code, detail) = self.code_and_detail();
        Reason {
            code,
            detail: detail.to_owned(),
            fp: None,
        }
    }
}

/// What a re-decision concluded, for `decide-only publish`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedecideState {
    /// Nothing to publish: a reply-only outcome, a moved head, or an unchanged decision (J9).
    Silent,
    /// The evidence is unusable: publish this refusal over the register's key (D11).
    Refused(Unusable),
    /// Publish the decision bundle written beside the state, whose sha256 this is.
    Decided {
        /// The sha256 of the bundle's exact bytes.
        digest: Sha256Digest,
    },
}

/// Why a re-decision state file was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedecideStateFileError {
    /// It is larger than [`MAX_STATE_BYTES`]; it was not parsed.
    TooLarge,
    /// It is not one JSON value, or an object in it repeats a key.
    Parse,
    /// It is JSON, but not exactly one of the states.
    Shape,
}

impl fmt::Display for RedecideStateFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TooLarge => "the re-decision state is over its size cap",
            Self::Parse => "the re-decision state is not JSON without repeated keys",
            Self::Shape => {
                "the re-decision state is not silent, refused or decided as decide-only writes it"
            }
        })
    }
}

impl Error for RedecideStateFileError {}

impl RedecideState {
    /// The state as [`STATE_FILE`](crate::STATE_FILE) holds it.
    #[must_use]
    pub fn json(&self) -> Vec<u8> {
        let value = match self {
            Self::Silent => json!({ "verdict": "silent" }),
            Self::Refused(unusable) => {
                let (code, detail) = unusable.code_and_detail();
                json!({
                    "verdict": "refused",
                    "reason": { "code": code.code(), "detail": detail },
                })
            }
            Self::Decided { digest } => json!({
                "verdict": "decided",
                "bundle_digest": digest.to_string(),
            }),
        };
        value.to_string().into_bytes()
    }

    /// The state `bytes` hold, exactly as [`Self::json`] writes one.
    ///
    /// # Errors
    ///
    /// [`RedecideStateFileError::TooLarge`] over [`MAX_STATE_BYTES`], unparsed;
    /// [`RedecideStateFileError::Parse`] for anything but JSON without repeated keys;
    /// [`RedecideStateFileError::Shape`] for anything but one state's keys and values.
    pub fn read(bytes: &[u8]) -> Result<Self, RedecideStateFileError> {
        if bytes.len() > MAX_STATE_BYTES {
            return Err(RedecideStateFileError::TooLarge);
        }
        let Value::Object(mut fields) =
            parse_json_strict(bytes).map_err(|_not_json| RedecideStateFileError::Parse)?
        else {
            return Err(RedecideStateFileError::Shape);
        };
        let state = match take_string(&mut fields, "verdict")?.as_str() {
            "silent" => Self::Silent,
            "refused" => {
                let Some(Value::Object(mut reason)) = fields.remove("reason") else {
                    return Err(RedecideStateFileError::Shape);
                };
                let code = take_string(&mut reason, "code")?;
                let detail = take_string(&mut reason, "detail")?;
                if !reason.is_empty() {
                    return Err(RedecideStateFileError::Shape);
                }
                let unusable = Unusable::ALL
                    .into_iter()
                    .find(|unusable| {
                        let (known, known_detail) = unusable.code_and_detail();
                        known.code() == code && known_detail == detail
                    })
                    .ok_or(RedecideStateFileError::Shape)?;
                Self::Refused(unusable)
            }
            "decided" => Self::Decided {
                digest: take_string(&mut fields, "bundle_digest")?
                    .parse()
                    .map_err(|_not_a_digest| RedecideStateFileError::Shape)?,
            },
            _ => return Err(RedecideStateFileError::Shape),
        };
        if fields.is_empty() {
            Ok(state)
        } else {
            Err(RedecideStateFileError::Shape)
        }
    }
}

/// Field `key` of `fields`, taken out; it must be a string.
fn take_string(
    fields: &mut Map<String, Value>,
    key: &str,
) -> Result<String, RedecideStateFileError> {
    match fields.remove(key) {
        Some(Value::String(text)) => Ok(text),
        _ => Err(RedecideStateFileError::Shape),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_reads_back_as_written() {
        let states = [
            RedecideState::Silent,
            RedecideState::Refused(Unusable::Forged),
            RedecideState::Refused(Unusable::Absent(Absence::Missing)),
            RedecideState::Refused(Unusable::Absent(Absence::Expired)),
            RedecideState::Decided {
                digest: Sha256Digest::of(b"run-decision"),
            },
        ];
        for state in states {
            assert_eq!(RedecideState::read(&state.json()), Ok(state));
        }
    }

    #[test]
    fn a_forged_evidence_refusal_is_gate_error_bundle_digest() {
        assert_eq!(
            RedecideState::Refused(Unusable::Forged).json(),
            br#"{"reason":{"code":"gate-error","detail":"bundle-digest"},"verdict":"refused"}"#
        );
    }

    #[test]
    fn a_reason_no_unusable_evidence_gives_is_refused() {
        for text in [
            r#"{"reason":{"code":"gate-error","detail":"artifact-digest"},"verdict":"refused"}"#,
            r#"{"reason":{"code":"bundle-expired","detail":"bundle-digest"},"verdict":"refused"}"#,
            r#"{"verdict":"silent","extra":1}"#,
            r#"{"verdict":"silent","verdict":"silent"}"#,
        ] {
            assert!(RedecideState::read(text.as_bytes()).is_err(), "{text}");
        }
    }
}
