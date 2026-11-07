//! Content hashes of named files: what makes a reviewer prompt bundle content-addressed
//! (`docs/architecture/04-pipeline.md` §3), so the evidence key changes with every byte of it
//! (`docs/architecture/02-architecture.md` §3).

use std::fmt;

use sha2::{Digest, Sha256};

/// The SHA-256 of a sequence of named files, computed by [`ContentHash::of_files`].
///
/// Displayed as `sha256:` and 64 lowercase hex digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    /// Hashes `files`, in the order given: for each, its name and then its content, each
    /// prefixed by its byte length as a big-endian `u64` (as the fingerprint is), so no byte can
    /// move between a name and a content, or between two files, without changing the hash.
    #[must_use]
    pub fn of_files<'a>(files: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> Self {
        let mut hasher = Sha256::new();
        for (name, content) in files {
            for field in [name.as_bytes(), content] {
                // `usize` is at most 64 bits on every target Rust supports, so this never
                // saturates.
                let len = u64::try_from(field.len()).unwrap_or(u64::MAX);
                hasher.update(len.to_be_bytes());
                hasher.update(field);
            }
        }
        Self(hasher.finalize().into())
    }

    /// The 32 bytes of the digest.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("sha256:")?;
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}
