//! Step 2, `exception`: the exception requests in the pinned `temper scan --all --json` output
//! (`docs/architecture/03-contracts.md` §2.1).
//!
//! temper prints a JSON array of hits, `{key, id, severity, line, text, msg, fix, exception?,
//! file}` (temper 1.3.0, `scanSource` plus the CLI's `file`), and exits 1 when a `hard` hit is
//! present. A hit covered by `#[expect(lint, reason = ..)]` is downgraded to `ask` and names
//! the lint in `exception`; temper's exit code ignores it, so assay checks it here.

use std::num::NonZeroU64;

use serde::Deserialize;

use super::report::json_detail;

/// A hit's level, as temper names it. Any other value fails the parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Level {
    Hard,
    Ask,
    Soft,
}

/// The fields of a hit the adapter reads; temper's other fields are ignored.
#[derive(Debug, Deserialize)]
struct HitDto {
    file: String,
    line: NonZeroU64,
    severity: Level,
    #[serde(default)]
    exception: Option<String>,
    #[serde(default)]
    text: Option<String>,
}

/// One exception request: an `ask` hit, or any hit naming an `exception`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Request {
    /// The file, relative to the scanned tree.
    pub(super) file: String,
    /// The line of the attribute or the covered code.
    pub(super) line: NonZeroU64,
    /// The lint excepted; `None` when temper named none, which nothing can admit.
    pub(super) lint: Option<String>,
    /// temper's quote of the line, when it gave one.
    pub(super) text: Option<String>,
}

/// The exception requests in `stdout`. Output that is not an array of hits with a non-empty
/// `file`, a line of at least 1 and a known level is refused with a detail that does not quote
/// it (the hits quote PR source).
pub(super) fn requests(stdout: &[u8]) -> Result<Vec<Request>, String> {
    let hits: Vec<HitDto> = serde_json::from_slice(stdout).map_err(|error| json_detail(&error))?;
    if hits.iter().any(|hit| hit.file.is_empty()) {
        return Err("a hit has an empty file".to_owned());
    }
    Ok(hits
        .into_iter()
        .filter(|hit| hit.severity == Level::Ask || hit.exception.is_some())
        .map(|hit| Request {
            file: hit.file,
            line: hit.line,
            lint: hit.exception,
            text: hit.text,
        })
        .collect())
}
