//! A lease document as a commit message: one JSON object in the form assay writes, read with
//! the ledger's strict record rules (03 §6c).

use serde::Serialize;

use super::refs::MAX_LEASE_MESSAGE_BYTES;
use crate::ledger::Corruption;

/// The commit message for the wire form `wire`: its JSON, with no final newline.
pub(super) fn to_message<S: Serialize>(wire: &S) -> Result<String, Corruption> {
    let message = serde_json::to_string(wire).map_err(|_shape| Corruption::Shape)?;
    if message.len() > MAX_LEASE_MESSAGE_BYTES {
        return Err(Corruption::LongLine);
    }
    Ok(message)
}

/// The document line of a commit message: refused past [`MAX_LEASE_MESSAGE_BYTES`], and with
/// one final newline removed (a git message's usual ending, which a host may add). The caller
/// checks that the line is exactly what [`to_message`] writes for the record it reads.
pub(super) fn line_of(message: &str) -> Result<&[u8], Corruption> {
    if message.len() > MAX_LEASE_MESSAGE_BYTES {
        return Err(Corruption::LongLine);
    }
    Ok(message.strip_suffix('\n').unwrap_or(message).as_bytes())
}
