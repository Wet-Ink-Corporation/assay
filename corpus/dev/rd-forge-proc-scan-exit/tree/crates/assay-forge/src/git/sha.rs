//! Immutable object names and the only remotes git may fetch from.

use std::fmt;
use std::str::FromStr;

/// Length of a full SHA-1 object name in hex digits.
const SHA_LEN: usize = 40;

/// The only scheme git may fetch over; `-c protocol.allow=never` refuses the rest as well.
const HTTPS: &str = "https://";

/// A full, lowercase SHA-1 commit name: the only way to name PR objects to fetch.
///
/// Branch names, `refs/pull/N/merge` and `head.ref` cannot be expressed, because they move
/// under the reviewer (`docs/architecture/02-architecture.md` §3, ADR-03).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CommitSha(String);

impl CommitSha {
    /// The 40 hex digits.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CommitSha {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why a string is not a [`CommitSha`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ParseShaError {
    /// Not exactly 40 lowercase hex digits: a ref, an abbreviation or not a name at all.
    #[error("a commit must be named by exactly 40 lowercase hex digits")]
    NotFortyHexDigits,
}

impl FromStr for CommitSha {
    type Err = ParseShaError;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        let lower_hex = |byte: &u8| matches!(byte, b'0'..=b'9' | b'a'..=b'f');
        if name.len() == SHA_LEN && name.as_bytes().iter().all(lower_hex) {
            Ok(Self(name.to_owned()))
        } else {
            Err(ParseShaError::NotFortyHexDigits)
        }
    }
}

/// An `https://` URL: the only kind of remote PR objects are fetched from.
///
/// Local paths, `file://`, `ssh://`, scp-style and `ext::` remotes cannot be expressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpsRemote(String);

impl HttpsRemote {
    /// The URL as given.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Why a string is not an [`HttpsRemote`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ParseRemoteError {
    /// The URL does not start with `https://` followed by a host.
    #[error("a remote must be an https:// URL with a host")]
    NotHttps,
    /// The URL contains whitespace or a control character.
    #[error("a remote must not contain whitespace or control characters")]
    ForbiddenCharacter,
}

impl FromStr for HttpsRemote {
    type Err = ParseRemoteError;

    fn from_str(url: &str) -> Result<Self, Self::Err> {
        let host_and_path = url
            .strip_prefix(HTTPS)
            .filter(|rest| !rest.is_empty())
            .ok_or(ParseRemoteError::NotHttps)?;
        if host_and_path
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(ParseRemoteError::ForbiddenCharacter);
        }
        Ok(Self(url.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "0671db2374a1928aa85a711de7476edd9263a48a";

    #[test]
    fn accepts_forty_lowercase_hex_digits() {
        let sha: CommitSha = SHA.parse().expect("a full SHA-1 is a commit name");

        assert_eq!(sha.as_str(), SHA, "the SHA must round-trip unchanged");
    }

    #[test]
    fn rejects_pull_request_refs() {
        assert_eq!(
            "refs/pull/52/merge".parse::<CommitSha>(),
            Err(ParseShaError::NotFortyHexDigits),
            "a moving ref must never name the object to fetch"
        );
    }

    #[test]
    fn rejects_abbreviated_and_uppercase_names() {
        assert_eq!(
            "0671db2374a1".parse::<CommitSha>(),
            Err(ParseShaError::NotFortyHexDigits),
            "an abbreviated SHA is ambiguous"
        );
        assert_eq!(
            SHA.to_uppercase().parse::<CommitSha>(),
            Err(ParseShaError::NotFortyHexDigits),
            "git prints lowercase; anything else is not a name git produced"
        );
    }

    #[test]
    fn accepts_https_remotes() {
        let remote: HttpsRemote = "https://github.com/Wet-Ink-Corporation/assay.git"
            .parse()
            .expect("an https URL is an allowed remote");

        assert_eq!(
            remote.as_str(),
            "https://github.com/Wet-Ink-Corporation/assay.git",
            "the URL must round-trip unchanged"
        );
    }

    #[test]
    fn rejects_every_transport_but_https() {
        for url in [
            "file:///tmp/repo",
            "/tmp/repo",
            "ssh://git@github.com/o/r",
            "git@github.com:o/r.git",
            "ext::sh -c touch% /tmp/pwned",
            "-uhttps://github.com/o/r",
            "https://",
        ] {
            assert_eq!(
                url.parse::<HttpsRemote>(),
                Err(ParseRemoteError::NotHttps),
                "{url} must be rejected"
            );
        }
    }

    #[test]
    fn rejects_urls_with_whitespace_or_control_characters() {
        assert_eq!(
            "https://github.com/o/r\n--upload-pack=x".parse::<HttpsRemote>(),
            Err(ParseRemoteError::ForbiddenCharacter),
            "a newline could smuggle a second line into git's config or logs"
        );
    }
}
