//! Immutable references to artifacts: plugin pins and OCI digests. Tags never parse.

use std::fmt;
use std::str::FromStr;

use sha2::{Digest, Sha256};
use thiserror::Error;

const SHA256_PREFIX: &str = "sha256:";
const COMMIT_HEX_LEN: usize = 40;
const SHA256_HEX_LEN: usize = 64;

/// `true` when `text` is exactly `len` lowercase hexadecimal digits.
fn is_lower_hex(text: &str, len: usize) -> bool {
    text.len() == len
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

/// Bytes displayed as lowercase hexadecimal, two digits each.
struct LowerHex<'b>(&'b [u8]);

impl fmt::Display for LowerHex<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

/// An OCI content digest, `sha256:<64 lowercase hex>`; a tag such as `latest` never parses.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Sha256Digest(String);

impl Sha256Digest {
    /// The SHA-256 digest of `bytes`: what a job publishes in its `outputs` for each artifact
    /// it uploads, and what Z4 recomputes before using one (02 §1, F18).
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        let digest = Sha256::digest(bytes);
        Self(format!("{SHA256_PREFIX}{}", LowerHex(&digest)))
    }

    /// The digest as written, `sha256:<hex>`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Sha256Digest {
    type Err = ParseDigestError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text.strip_prefix(SHA256_PREFIX) {
            Some(hex) if is_lower_hex(hex, SHA256_HEX_LEN) => Ok(Self(text.to_owned())),
            _ => Err(ParseDigestError),
        }
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A full git commit id: 40 lowercase hexadecimal digits.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommitId(String);

impl CommitId {
    /// The commit id as 40 hexadecimal digits.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for CommitId {
    type Err = ParseCommitIdError;

    /// Parses a full commit id; an abbreviated, uppercase or prefixed one never parses.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if is_lower_hex(text, COMMIT_HEX_LEN) {
            Ok(Self(text.to_owned()))
        } else {
            Err(ParseCommitIdError)
        }
    }
}

impl fmt::Display for CommitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where a built-in adapter's tool comes from, by immutable identity only
/// (`docs/architecture/03-contracts.md` §1): `git:<40-hex>` or `oci:sha256:<64-hex>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Pin {
    /// A git commit of the tool's repository.
    Git(CommitId),
    /// An OCI artifact by content digest.
    Oci(Sha256Digest),
}

impl FromStr for Pin {
    type Err = ParsePinError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if let Some(commit) = text.strip_prefix("git:") {
            return if is_lower_hex(commit, COMMIT_HEX_LEN) {
                Ok(Self::Git(CommitId(commit.to_owned())))
            } else {
                Err(ParsePinError::Tag)
            };
        }
        if let Some(reference) = text.strip_prefix("oci:") {
            return reference
                .parse()
                .map(Self::Oci)
                .map_err(|ParseDigestError| ParsePinError::Tag);
        }
        Err(ParsePinError::UnknownScheme)
    }
}

impl fmt::Display for Pin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Git(commit) => write!(f, "git:{commit}"),
            Self::Oci(digest) => write!(f, "oci:{digest}"),
        }
    }
}

/// Why a pin was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ParsePinError {
    /// The pin names a known scheme but not a full digest: a tag, a branch, an abbreviated or
    /// uppercase commit id. Mutable references are refused, never resolved.
    #[error("a pin must be a full lowercase digest, never a tag or branch")]
    Tag,
    /// The pin starts with neither `git:` nor `oci:`.
    #[error("a pin must start with `git:` or `oci:`")]
    UnknownScheme,
}

/// A commit id that is not 40 lowercase hexadecimal digits, such as a branch name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("a commit id must be 40 lowercase hexadecimal digits")]
pub struct ParseCommitIdError;

/// A digest that is not `sha256:<64 lowercase hex>`, such as a tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("a digest must be `sha256:` and 64 lowercase hexadecimal digits, never a tag")]
pub struct ParseDigestError;

#[cfg(test)]
mod tests {
    use super::Sha256Digest;

    #[test]
    fn the_digest_of_bytes_is_their_sha256_in_lowercase_hex() {
        // FIPS 180-2 test vectors: the empty message and "abc".
        let digests = [Sha256Digest::of(b""), Sha256Digest::of(b"abc")];

        assert_eq!(
            digests.map(|digest| digest.to_string()),
            [
                "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ]
            .map(str::to_owned)
        );
    }
}
