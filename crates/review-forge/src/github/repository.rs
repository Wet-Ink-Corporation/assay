//! A repository's `owner/name`, validated so that it can be written into an API path as is.

use std::fmt;
use std::str::FromStr;

/// The longest owner or name GitHub allows.
const MAX_PART_BYTES: usize = 100;

/// What a github.com remote URL starts with, before `owner/name`.
const REMOTE_PREFIXES: [&str; 3] = [
    "https://github.com/",
    "git@github.com:",
    "ssh://git@github.com/",
];

/// The suffix a remote URL may add to the repository's name.
const GIT_SUFFIX: &str = ".git";

/// A repository as `owner/name`: each part 1 to 100 of `A-Z a-z 0-9 . _ -`, neither `.` nor
/// `..`, so it never needs encoding and never changes the path it is written into.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Repository {
    owner: String,
    name: String,
}

impl Repository {
    /// The owner.
    #[must_use]
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// The name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The repository a git remote URL names: exactly `https://github.com/<owner>/<name>`,
    /// `git@github.com:<owner>/<name>` or `ssh://git@github.com/<owner>/<name>`, each with an
    /// optional `.git`. Anything else (another host, a port, credentials, a trailing `/`, a
    /// deeper path, a name still ending in `.git`) is refused rather than guessed at
    /// (wi-b5affa).
    ///
    /// # Errors
    ///
    /// [`ParseRepositoryError`] for any other URL, or an owner or name [`FromStr`] refuses.
    pub fn from_remote_url(url: &str) -> Result<Self, ParseRepositoryError> {
        let path = REMOTE_PREFIXES
            .iter()
            .find_map(|prefix| url.strip_prefix(prefix))
            .ok_or(ParseRepositoryError)?;
        let full_name = path.strip_suffix(GIT_SUFFIX).unwrap_or(path);
        if full_name.ends_with(GIT_SUFFIX) {
            return Err(ParseRepositoryError);
        }
        full_name.parse()
    }

    /// Whether `full_name` (`owner/name`, as GitHub returns it) is this repository; GitHub
    /// compares names without case.
    pub(crate) fn is(&self, full_name: &str) -> bool {
        full_name.split_once('/').is_some_and(|(owner, name)| {
            owner.eq_ignore_ascii_case(&self.owner) && name.eq_ignore_ascii_case(&self.name)
        })
    }
}

/// Why a repository name was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "a repository must be `owner/name`, each 1 to 100 of `A-Z a-z 0-9 . _ -` and neither `.` nor `..`"
)]
pub struct ParseRepositoryError;

impl FromStr for Repository {
    type Err = ParseRepositoryError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (owner, name) = text.split_once('/').ok_or(ParseRepositoryError)?;
        if is_part(owner) && is_part(name) {
            Ok(Self {
                owner: owner.to_owned(),
                name: name.to_owned(),
            })
        } else {
            Err(ParseRepositoryError)
        }
    }
}

impl fmt::Display for Repository {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

/// Whether `part` is a valid owner or name.
pub(crate) fn is_part(part: &str) -> bool {
    (1..=MAX_PART_BYTES).contains(&part.len())
        && part != "."
        && part != ".."
        && part
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_owner_and_name() {
        let repository: Repository = "Wet-Ink-Corporation/assay".parse().expect("valid");

        assert_eq!(
            (repository.owner(), repository.name()),
            ("Wet-Ink-Corporation", "assay")
        );
        assert_eq!(repository.to_string(), "Wet-Ink-Corporation/assay");
    }

    #[test]
    fn refuses_anything_that_could_change_a_path() {
        for text in [
            "acme",
            "acme/",
            "/widgets",
            "acme/widgets/extra",
            "acme/..",
            "./widgets",
            "acme/wid gets",
            "acme/widgets?x=1",
            "acme/wid%2Fgets",
        ] {
            assert_eq!(
                text.parse::<Repository>(),
                Err(ParseRepositoryError),
                "{text}"
            );
        }
    }

    #[test]
    fn a_github_remote_url_names_its_repository() {
        for url in [
            "https://github.com/acme/widgets",
            "https://github.com/acme/widgets.git",
            "git@github.com:acme/widgets.git",
            "git@github.com:acme/widgets",
            "ssh://git@github.com/acme/widgets.git",
        ] {
            assert_eq!(
                Repository::from_remote_url(url),
                Ok(Repository {
                    owner: "acme".to_owned(),
                    name: "widgets".to_owned(),
                }),
                "{url}"
            );
        }
    }

    #[test]
    fn a_remote_url_that_is_not_plainly_github_is_refused() {
        for url in [
            "",
            "https://github.com/acme/widgets/",
            "https://github.com/acme/widgets.git.git",
            "https://github.com/acme",
            "https://github.com/acme/widgets/tree/main",
            "https://user@github.com/acme/widgets",
            "https://github.com:443/acme/widgets",
            "https://github.com.evil.example/acme/widgets",
            "https://gitlab.com/acme/widgets",
            "http://github.com/acme/widgets",
            "HTTPS://github.com/acme/widgets",
            "ssh://github.com/acme/widgets",
            "git@github.com/acme/widgets",
            "github.com:acme/widgets",
            "/srv/git/widgets.git",
            "https://github.com/acme/..",
            "https://github.com/acme/.git",
        ] {
            assert_eq!(
                Repository::from_remote_url(url),
                Err(ParseRepositoryError),
                "{url}"
            );
        }
    }

    #[test]
    fn full_names_compare_without_case() {
        let repository: Repository = "acme/widgets".parse().expect("valid");

        assert!(repository.is("ACME/Widgets"), "case-insensitive");
        assert!(!repository.is("acme/widgets-fork"), "another repository");
    }
}
