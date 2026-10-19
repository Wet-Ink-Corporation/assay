//! The ledger repository adapter: `assay-app`'s [`Ledger`] over the GitHub contents API, with
//! the `assay-ledger-writer` App's installation token (06 §1, ADR-26).
//!
//! A file is read with `GET contents/<path>` (and, past 1 MB, its blob with `GET git/blobs/<sha>`)
//! and written with `PUT contents/<path>` carrying the `sha` read, which GitHub refuses with 409
//! when the file moved on: that refusal is the compare-and-set. Everything about the records in
//! the files is `assay-app`'s; this adapter moves bytes.

mod base64;
mod wire;

pub(super) use base64::encode as encode_base64;

use assay_app::{
    FileVersion, Ledger, LedgerFile, LedgerPath, MAX_LEDGER_FILE_BYTES, WriteCondition,
    WriteOutcome,
};
use serde_json::{Map, Value};

use super::api::{Api, Failed, encode, percent_encode};
use super::api_path;
use super::error::{ForgeError, Operation};
use super::repository::Repository;
use super::transport::{ApiPath, Request, Sleep, Transport};
use wire::{Blob, ContentsFile, RepositoryView};

/// The ledger repository through the contents API.
#[derive(Debug)]
pub struct LedgerRepo<T, S> {
    api: Api<T, S>,
    repository: Repository,
}

/// Why the ledger repository could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum LedgerRepoError<E> {
    /// GitHub could not be reached, refused the request, or answered in a shape the adapter
    /// does not read.
    #[error(transparent)]
    Forge(#[from] ForgeError<E>),
    /// The file is over [`MAX_LEDGER_FILE_BYTES`]: it was neither fetched whole nor decoded, or,
    /// for a write, not sent.
    #[error("ledger file {path} is {size} bytes, over the {MAX_LEDGER_FILE_BYTES}-byte cap")]
    TooLarge {
        /// The file.
        path: LedgerPath,
        /// Its length in bytes.
        size: u64,
    },
    /// An attempt to write the file failed without an answer, and the file then moved on or a
    /// resend was refused as stale: the write may have landed and been overwritten since, so
    /// the file may or may not hold it. Never a conflict, which promises nothing was written.
    #[error("ledger file {path} may or may not have been written")]
    Unconfirmed {
        /// The file.
        path: LedgerPath,
    },
}

/// What a read after a failed write shows.
enum Probe {
    /// The file holds what was written: the write landed.
    Landed,
    /// The file is still as the write's condition says: it can be sent again.
    Unchanged,
    /// The file is neither: the write did not land, or landed and was overwritten since.
    Moved,
}

/// How one attempt of a write ended.
enum Sent {
    /// The write landed.
    Written,
    /// The file was not at the condition's version, and no earlier attempt may have landed.
    Conflict,
    /// The file moved after an attempt that may have landed.
    Unconfirmed,
}

impl<T: Transport, S: Sleep> LedgerRepo<T, S> {
    /// The ledger repository `repository`, called through `api`, whose transport must carry an
    /// `assay-ledger-writer` installation token (`contents: write` to read and write).
    pub const fn new(api: Api<T, S>, repository: Repository) -> Self {
        Self { api, repository }
    }

    /// `repos/<owner>/<name>/<rest>`, for `operation`.
    fn repo_path<E>(&self, operation: Operation, rest: &str) -> Result<ApiPath, ForgeError<E>> {
        api_path(operation, format!("repos/{}/{rest}", self.repository))
    }

    /// `repos/<owner>/<name>/contents/<path>`, each segment percent-encoded.
    fn contents_path<E>(
        &self,
        operation: Operation,
        path: &LedgerPath,
    ) -> Result<ApiPath, ForgeError<E>> {
        let segments: Vec<String> = path.as_str().split('/').map(percent_encode).collect();
        self.repo_path(operation, &format!("contents/{}", segments.join("/")))
    }

    /// The file at `path`, its size checked before anything is decoded.
    fn fetch(&self, path: &LedgerPath) -> Result<Option<LedgerFile>, LedgerRepoError<T::Error>> {
        let operation = Operation::ReadLedgerFile;
        let malformed = |detail| ForgeError::Malformed { operation, detail };
        let Some(file) = self
            .api
            .get_optional::<ContentsFile>(operation, &self.contents_path(operation, path)?)?
        else {
            self.confirm_repository()?;
            return Ok(None);
        };
        if file.kind != "file" {
            return Err(malformed("the ledger path is not a file").into());
        }
        if file.path != path.as_str() {
            return Err(malformed("the contents API answered for another path").into());
        }
        check_size(path, file.size)?;
        check_sha(operation, &file.sha)?;
        let content = match file.encoding.as_str() {
            "base64" => file.content,
            "none" => self.blob(&file.sha, file.size)?,
            _ => return Err(malformed("unknown content encoding").into()),
        };
        let contents = decode_content(operation, &content, file.size)?;
        Ok(Some(LedgerFile {
            version: FileVersion::new(file.sha),
            contents,
        }))
    }

    /// Checks that the token sees the ledger repository, so that a 404 for a file means the file
    /// is absent.
    fn confirm_repository(&self) -> Result<(), ForgeError<T::Error>> {
        confirm_repository(&self.api, &self.repository, Operation::ReadLedgerFile)
    }

    /// The base64 content of blob `sha`, which the contents listing gave as `size` bytes.
    fn blob(&self, sha: &str, size: u64) -> Result<String, ForgeError<T::Error>> {
        let operation = Operation::ReadLedgerFile;
        let target = self.repo_path(operation, &format!("git/blobs/{sha}"))?;
        let blob: Blob = self.api.get(operation, &target)?;
        if blob.sha != sha || blob.size != size || blob.encoding != "base64" {
            return Err(ForgeError::Malformed {
                operation,
                detail: "the blob is not the one the contents API listed",
            });
        }
        Ok(blob.content)
    }

    /// What `path` shows after a write of `contents` under `condition` failed retryably.
    fn probe(
        &self,
        path: &LedgerPath,
        contents: &[u8],
        condition: &WriteCondition,
    ) -> Result<Probe, ForgeError<T::Error>> {
        let file = match self.fetch(path) {
            Ok(file) => file,
            Err(LedgerRepoError::Forge(error)) => return Err(error),
            Err(LedgerRepoError::TooLarge { .. } | LedgerRepoError::Unconfirmed { .. }) => {
                return Ok(Probe::Moved);
            }
        };
        Ok(match (file, condition) {
            (Some(found), _) if found.contents == contents => Probe::Landed,
            (None, WriteCondition::Absent) => Probe::Unchanged,
            (Some(found), WriteCondition::At(version)) if found.version == *version => {
                Probe::Unchanged
            }
            _ => Probe::Moved,
        })
    }
}

impl<T: Transport, S: Sleep> Ledger for LedgerRepo<T, S> {
    type Error = LedgerRepoError<T::Error>;

    fn read(&self, path: &LedgerPath) -> Result<Option<LedgerFile>, Self::Error> {
        self.fetch(path)
    }

    /// `PUT contents/<path>` with the version read as `sha` (none to create). GitHub's 409 is a
    /// conflict, and so is its 422 for a create over an existing file (`"sha" wasn't supplied`).
    ///
    /// After a retryable failure the file is read first: holding the contents, the write landed;
    /// still at the condition's version, it is sent again, and GitHub's check of the `sha` lets
    /// at most one of the sends land. Once an attempt failed without an answer, a moved file or
    /// a resend refused as stale is [`LedgerRepoError::Unconfirmed`], never
    /// [`WriteOutcome::Conflict`]: the write may have landed and been overwritten, and a caller
    /// that re-applied its change on a conflict would apply it twice.
    fn write(
        &self,
        path: &LedgerPath,
        contents: &[u8],
        condition: &WriteCondition,
    ) -> Result<WriteOutcome, Self::Error> {
        if contents.len() > MAX_LEDGER_FILE_BYTES {
            return Err(LedgerRepoError::TooLarge {
                path: path.clone(),
                size: u64::try_from(contents.len()).unwrap_or(u64::MAX),
            });
        }
        let operation = Operation::WriteLedgerFile;
        let body = encode(operation, &put_body(path, contents, condition))?;
        let target = self.contents_path(operation, path)?;
        let request = Request::put(&target, &body);
        let sent = self.api.retrying(operation, |attempt| {
            let resend = attempt > 0;
            if resend {
                match self.probe(path, contents, condition)? {
                    Probe::Landed => return Ok(Sent::Written),
                    Probe::Moved => return Ok(Sent::Unconfirmed),
                    Probe::Unchanged => {}
                }
            }
            match self.api.attempt(operation, &request) {
                Ok(_response) => Ok(Sent::Written),
                Err(Failed::Stop(error)) if is_stale(&error, condition) => Ok(if resend {
                    Sent::Unconfirmed
                } else {
                    Sent::Conflict
                }),
                Err(failed) => Err(failed),
            }
        })?;
        match sent {
            Sent::Written => Ok(WriteOutcome::Written),
            Sent::Conflict => Ok(WriteOutcome::Conflict),
            Sent::Unconfirmed => Err(LedgerRepoError::Unconfirmed { path: path.clone() }),
        }
    }
}

/// Checks that `api`'s token sees `repository`, so that a 404 inside it means the file or ref is
/// absent. GitHub answers 404 for a private repository the token cannot read too, and reading
/// that as "no cost records" or "a free slot" would let a misconfigured job spend unbudgeted
/// (wi-b5affa).
pub(super) fn confirm_repository<T: Transport, S: Sleep>(
    api: &Api<T, S>,
    repository: &Repository,
    operation: Operation,
) -> Result<(), ForgeError<T::Error>> {
    let root = api_path(operation, format!("repos/{repository}"))?;
    let seen: RepositoryView = api.get(operation, &root)?;
    let same = seen.full_name.split_once('/').is_some_and(|(owner, name)| {
        owner.eq_ignore_ascii_case(repository.owner())
            && name.eq_ignore_ascii_case(repository.name())
    });
    if same {
        Ok(())
    } else {
        Err(ForgeError::Malformed {
            operation,
            detail: "the API answered for another repository",
        })
    }
}

/// Whether GitHub refused a write under `condition` because the file is not at its version:
/// 409 for a stale `sha`, and 422 for a create over an existing file.
fn is_stale<E>(error: &ForgeError<E>, condition: &WriteCondition) -> bool {
    match error {
        ForgeError::Rejected { status: 409, .. } => true,
        ForgeError::Rejected { status: 422, .. } => *condition == WriteCondition::Absent,
        _ => false,
    }
}

/// The `PUT` body: a commit message naming the file, the contents in base64, and the version
/// read as `sha` unless the file is created.
fn put_body(path: &LedgerPath, contents: &[u8], condition: &WriteCondition) -> Value {
    let mut body = Map::new();
    body.insert(
        "message".to_owned(),
        Value::String(format!("assay-ledger: update {path}")),
    );
    body.insert(
        "content".to_owned(),
        Value::String(base64::encode(contents)),
    );
    if let WriteCondition::At(version) = condition {
        body.insert("sha".to_owned(), Value::String(version.as_str().to_owned()));
    }
    Value::Object(body)
}

/// Refuses a file listed over [`MAX_LEDGER_FILE_BYTES`].
fn check_size<E>(path: &LedgerPath, size: u64) -> Result<(), LedgerRepoError<E>> {
    let cap = u64::try_from(MAX_LEDGER_FILE_BYTES).unwrap_or(u64::MAX);
    if size > cap {
        return Err(LedgerRepoError::TooLarge {
            path: path.clone(),
            size,
        });
    }
    Ok(())
}

/// Refuses a version that is not a full git blob sha (40 lowercase hexadecimal digits).
fn check_sha<E>(operation: Operation, sha: &str) -> Result<(), ForgeError<E>> {
    let full = sha.len() == 40
        && sha
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'));
    if full {
        Ok(())
    } else {
        Err(ForgeError::Malformed {
            operation,
            detail: "the file's sha is not a full blob id",
        })
    }
}

/// The bytes of base64 `content`, which must decode to exactly `size` bytes.
fn decode_content<E>(
    operation: Operation,
    content: &str,
    size: u64,
) -> Result<Vec<u8>, ForgeError<E>> {
    let malformed = |detail| ForgeError::Malformed { operation, detail };
    let bytes = base64::decode(content).ok_or_else(|| malformed("the content is not base64"))?;
    if u64::try_from(bytes.len()).ok() != Some(size) {
        return Err(malformed("the content's length differs from its size"));
    }
    Ok(bytes)
}
