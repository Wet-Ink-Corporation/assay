//! Per-job nonce artifact names (`docs/architecture/02-architecture.md` §1, `05` T11).

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// Hex digits in a nonce: 128 bits.
const NONCE_HEX_LEN: usize = 32;

/// Where fresh nonces come from: an unpredictable source in production, so no other workflow
/// can know a run's artifact names before it starts.
pub trait NonceSource {
    /// Why no nonce could be drawn.
    type Error: std::error::Error;

    /// A fresh nonce.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the source cannot produce one.
    fn nonce(&mut self) -> Result<ArtifactNonce, Self::Error>;
}

/// A nonce for one job's artifact name: 32 lowercase hexadecimal digits (128 bits).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ArtifactNonce(String);

impl FromStr for ArtifactNonce {
    type Err = ParseNonceError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let hex = text.len() == NONCE_HEX_LEN
            && text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'));
        if hex {
            Ok(Self(text.to_owned()))
        } else {
            Err(ParseNonceError)
        }
    }
}

impl fmt::Display for ArtifactNonce {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A nonce that is not 32 lowercase hexadecimal digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("a nonce must be 32 lowercase hexadecimal digits")]
pub struct ParseNonceError;

#[cfg(test)]
mod tests {
    use super::*;

    const NONCE: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn a_nonce_round_trips_through_display() {
        assert_eq!(
            NONCE
                .parse::<ArtifactNonce>()
                .map(|nonce| nonce.to_string()),
            Ok(NONCE.to_owned())
        );
    }

    #[test]
    fn a_nonce_is_exactly_32_lowercase_hex_digits() {
        let short = NONCE.get(1..).unwrap_or_default().to_owned();
        for text in [
            short,
            format!("{NONCE}0"),
            NONCE.to_uppercase(),
            format!("g{}", NONCE.get(1..).unwrap_or_default()),
            String::new(),
        ] {
            assert_eq!(
                text.parse::<ArtifactNonce>(),
                Err(ParseNonceError),
                "{text:?} is not a nonce"
            );
        }
    }
}
