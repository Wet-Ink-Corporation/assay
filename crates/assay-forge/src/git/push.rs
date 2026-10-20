//! Deleting a ref of a remote by compare-and-set: `git push --porcelain
//! --force-with-lease=<ref>:<expected> -- <remote> :<ref>` (06 §1, A-33). The remote's
//! receive-pack applies the deletion only while the ref still points at `<expected>`, atomically,
//! so a holder whose lease was reaped never deletes a newer holder's.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use super::GitError;
use super::command::{GitOperation, HttpHeader, Scope};
use super::repository::{Call, ObjectRepo, failed};
use super::sha::{CommitSha, HttpsRemote, ParseRemoteError};

/// What every [`BranchRef`] starts with.
const HEADS: &str = "refs/heads/";

/// The summary `--porcelain` prints for a ref the lease check refused.
const STALE: &str = "[rejected] (stale info)";

/// The suffix git reserves for its lock files, refused in any ASCII case.
const LOCK_SUFFIX: &str = ".lock";

/// A branch ref, `refs/heads/<segments>`.
///
/// Each segment is ASCII letters, digits, `-`, `_` or `.`, does not start with `.` or `-`, holds
/// no `..` and does not end in `.lock`: nothing in it can read as an option, a refspec separator
/// or a revision expression.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BranchRef(String);

impl BranchRef {
    /// The full ref name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BranchRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A name that is not a [`BranchRef`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a ref must be refs/heads/ followed by plain path segments")]
pub struct ParseRefError;

impl FromStr for BranchRef {
    type Err = ParseRefError;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        let rest = name.strip_prefix(HEADS).ok_or(ParseRefError)?;
        let plain = |segment: &str| {
            !segment.is_empty()
                && !segment.starts_with(['.', '-'])
                && !segment
                    .get(segment.len().saturating_sub(LOCK_SUFFIX.len())..)
                    .is_some_and(|tail| tail.eq_ignore_ascii_case(LOCK_SUFFIX))
                && !segment.contains("..")
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        };
        if rest.split('/').all(plain) {
            Ok(Self(name.to_owned()))
        } else {
            Err(ParseRefError)
        }
    }
}

/// An `Authorization` header line for an https push, `Authorization: <scheme> <credentials>`.
/// Its [`Debug`](fmt::Debug) never shows the credentials.
#[derive(Clone, PartialEq, Eq)]
pub struct Authorization(String);

/// A header line with a control character, which could end the header and start another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("an authorization header must be one line of printable characters")]
pub struct InvalidHeader;

impl Authorization {
    /// The header line `line`, for example `Authorization: basic <base64>`.
    ///
    /// # Errors
    ///
    /// [`InvalidHeader`] when `line` holds a control character (a CR or LF would let it carry
    /// another header).
    pub fn new(line: impl Into<String>) -> Result<Self, InvalidHeader> {
        let line = line.into();
        if line.chars().any(char::is_control) {
            return Err(InvalidHeader);
        }
        Ok(Self(line))
    }
}

impl fmt::Debug for Authorization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Authorization(<redacted>)")
    }
}

/// Where a ref deletion is pushed: an https remote with its credential header, or a local bare
/// repository (a mirror on this host, and the tests' ledgers).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushRemote(Target);

#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    Https {
        remote: HttpsRemote,
        authorization: Authorization,
    },
    Local(String),
}

impl PushRemote {
    /// `remote`, sent `authorization` and nothing else; the header goes to that URL only, and no
    /// redirect is followed.
    #[must_use]
    pub const fn https(remote: HttpsRemote, authorization: Authorization) -> Self {
        Self(Target::Https {
            remote,
            authorization,
        })
    }

    /// The bare repository at `path`, pushed to over the file protocol.
    ///
    /// # Errors
    ///
    /// [`ParseRemoteError::NotAbsolute`] for a relative path, and
    /// [`ParseRemoteError::ForbiddenCharacter`] for one that is not UTF-8 or holds a control
    /// character.
    pub fn local_bare(path: impl Into<PathBuf>) -> Result<Self, ParseRemoteError> {
        let path = path.into();
        if !path.is_absolute() {
            return Err(ParseRemoteError::NotAbsolute);
        }
        let text = path
            .into_os_string()
            .into_string()
            .map_err(|_not_utf8| ParseRemoteError::ForbiddenCharacter)?;
        if text.chars().any(char::is_control) {
            return Err(ParseRemoteError::ForbiddenCharacter);
        }
        Ok(Self(Target::Local(text)))
    }
}

/// What a compare-and-set deletion did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeleteOutcome {
    /// The ref pointed at the expected commit and is gone.
    Deleted,
    /// The ref pointed elsewhere or did not exist; the remote kept it as it was.
    Stale,
}

impl ObjectRepo {
    /// Deletes `name` on `remote` only while it points at `expected`, with
    /// `git push --porcelain --force-with-lease=<name>:<expected> -- <remote> :<name>`.
    ///
    /// No object is sent, so the repository may be empty.
    ///
    /// # Errors
    ///
    /// [`GitError::Failed`] when the push fails for any other reason than the lease check (the
    /// remote refused the deletion, could not be reached or the credential was refused). The
    /// deletion is then usually not made, but git can also exit non-zero after reporting the
    /// ref deleted, and that is reported as `Failed` too (fail closed, not mapped to
    /// [`DeleteOutcome::Deleted`]): a caller treats the ref as possibly deleted.
    /// [`GitError::Malformed`] when git reported success without reporting the deletion;
    /// [`GitError::Process`] when it could not be run to completion, after which the ref may or
    /// may not have been deleted.
    pub fn delete_ref_if(
        &self,
        remote: &PushRemote,
        name: &BranchRef,
        expected: &CommitSha,
    ) -> Result<DeleteOutcome, GitError> {
        let (operation, url, header) = match &remote.0 {
            Target::Https {
                remote,
                authorization,
            } => (
                GitOperation::Push,
                remote.as_str(),
                Some(HttpHeader {
                    url: remote.as_str(),
                    line: &authorization.0,
                }),
            ),
            Target::Local(path) => (GitOperation::PushLocal, path.as_str(), None),
        };
        let lease = format!("--force-with-lease={name}:{expected}");
        let deletion = format!(":{name}");
        let args = [lease.as_str(), "--", url, deletion.as_str()];
        let captured = self.run_to_exit(Call {
            scope: Scope {
                header,
                ..Scope::default()
            },
            ..self.call(operation, &args)
        })?;
        match (report(&captured.stdout, name), captured.status.success()) {
            (Report::Deleted, true) => Ok(DeleteOutcome::Deleted),
            (Report::Stale, false) => Ok(DeleteOutcome::Stale),
            (_, false) => Err(failed(operation, &captured)),
            (_, true) => Err(GitError::Malformed {
                operation,
                detail: "the push did not report the ref deleted",
            }),
        }
    }
}

/// What `--porcelain` reported for one ref.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Report {
    /// `-`: deleted.
    Deleted,
    /// `!` with the lease check's summary.
    Stale,
    /// Anything else, or no line for the ref.
    Other,
}

/// The report on `name` in `--porcelain` output: lines `<flag>\t<from>:<to>\t<summary>`
/// between `To <url>` and `Done`.
fn report(stdout: &[u8], name: &BranchRef) -> Report {
    let Ok(text) = str::from_utf8(stdout) else {
        return Report::Other;
    };
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let (flag, refs, summary) = (fields.next()?, fields.next()?, fields.next()?);
            let (_from, to) = refs.rsplit_once(':')?;
            (to == name.as_str() && fields.next().is_none()).then_some((flag, summary))
        })
        .map(|(flag, summary)| match (flag, summary) {
            ("-", _) => Report::Deleted,
            ("!", STALE) => Report::Stale,
            _ => Report::Other,
        })
        .next()
        .unwrap_or(Report::Other)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsStr;
    use std::path::Path;

    use super::*;
    use crate::git::GitEnvironment;
    use crate::git::command::git_in;

    fn name() -> BranchRef {
        "refs/heads/slot-0".parse().expect("a plain head")
    }

    #[test]
    fn porcelain_reports_are_read_for_the_ref_pushed() {
        let to = "To https://github.com/o/ledger.git\n";
        let deleted = format!("{to}-\t:refs/heads/slot-0\t[deleted]\nDone\n");
        let stale = format!("{to}!\t(delete):refs/heads/slot-0\t{STALE}\nDone\n");
        let refused =
            format!("{to}!\t(delete):refs/heads/slot-0\t[remote rejected] (declined)\nDone\n");
        let other_ref = format!("{to}-\t:refs/heads/slot-1\t[deleted]\nDone\n");

        let reports = [&deleted, &stale, &refused, &other_ref, &String::new()]
            .map(|stdout| report(stdout.as_bytes(), &name()));

        assert_eq!(
            reports,
            [
                Report::Deleted,
                Report::Stale,
                Report::Other,
                Report::Other,
                Report::Other
            ]
        );
    }

    #[test]
    fn an_https_push_carries_its_credential_in_the_environment_only() {
        let environment = GitEnvironment::from_process_env().expect("the test host has a kill");
        let header = HttpHeader {
            url: "https://github.com/o/ledger.git",
            line: "Authorization: basic c2VjcmV0",
        };
        let scope = Scope {
            header: Some(header),
            ..Scope::default()
        };

        let command = git_in(Path::new("/p.git"), scope, &environment, GitOperation::Push);

        let args: Vec<&OsStr> = command.get_args().collect();
        assert!(
            args.iter()
                .all(|arg| !arg.to_string_lossy().contains("c2VjcmV0")),
            "{args:?}"
        );
        let envs: BTreeMap<&OsStr, Option<&OsStr>> = command.get_envs().collect();
        assert_eq!(
            ["GIT_CONFIG_COUNT", "GIT_CONFIG_KEY_0", "GIT_CONFIG_VALUE_0"]
                .map(|key| envs.get(OsStr::new(key)).copied().flatten()),
            [
                Some(OsStr::new("1")),
                Some(OsStr::new(
                    "http.https://github.com/o/ledger.git.extraHeader"
                )),
                Some(OsStr::new("Authorization: basic c2VjcmV0")),
            ]
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == [OsStr::new("-c"), OsStr::new("http.followRedirects=false")]),
            "{args:?}"
        );
    }

    #[test]
    fn only_a_push_carries_a_header() {
        let environment = GitEnvironment::from_process_env().expect("the test host has a kill");
        let scope = Scope {
            header: Some(HttpHeader {
                url: "https://github.com/o/r.git",
                line: "Authorization: basic c2VjcmV0",
            }),
            ..Scope::default()
        };

        let command = git_in(
            Path::new("/p.git"),
            scope,
            &environment,
            GitOperation::Fetch,
        );

        assert!(
            command
                .get_envs()
                .all(|(key, _)| key != OsStr::new("GIT_CONFIG_COUNT"))
        );
    }

    #[test]
    fn a_header_line_is_one_line() {
        assert_eq!(
            Authorization::new("Authorization: basic x\r\nX-Evil: 1").map(drop),
            Err(InvalidHeader)
        );
        let accepted = Authorization::new("Authorization: basic c2VjcmV0").expect("one line");
        assert_eq!(format!("{accepted:?}"), "Authorization(<redacted>)");
    }
}
