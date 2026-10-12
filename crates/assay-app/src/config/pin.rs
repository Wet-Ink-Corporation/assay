//! Immutable references to artifacts: plugin pins and OCI digests. Tags never parse.

use std::fmt;
use std::str::FromStr;

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

/// An OCI content digest, `sha256:<64 lowercase hex>`; a tag such as `latest` never parses.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Sha256Digest(String);

impl Sha256Digest {
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
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CommitId(String);

impl CommitId {
    /// The commit id as 40 hexadecimal digits.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
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

/// A digest that is not `sha256:<64 lowercase hex>`, such as a tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("a digest must be `sha256:` and 64 lowercase hexadecimal digits, never a tag")]
pub struct ParseDigestError;
