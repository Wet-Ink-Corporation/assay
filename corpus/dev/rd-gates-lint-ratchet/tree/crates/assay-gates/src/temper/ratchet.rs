//! Step 5: what the pinned `temper baseline --check --findings-out` wrote, read strictly
//! (`docs/architecture/03-contracts.md` §2.1).
//!
//! temper 1.3.0 writes its collection (`collectFindings`, `lib/findings.mjs`) with the ratchet
//! result (`ratchet`, `lib/baseline.mjs`) under `ratchet`. When `git diff` throws, `ratchet`
//! silently compares only the totals: `mode` becomes `totals`, `base` `null`, a note says "only
//! the totals were compared", and `ok` can still be true. So the ratchet is believed only in
//! `diff+totals` mode, with no such note, against exactly the merge base assay computed.
//!
//! Anything else temper 1.3.0 does not write is refused: a repeated key, an unknown field, an
//! unknown severity or tool, a line of 0 (refuse rather than model, wi-b5affa).

use std::fmt;
use std::num::NonZeroU64;

use serde::Deserialize;
use serde::de::IgnoredAny;

use super::report::json_detail;

/// The only collection format this adapter reads (`"temper_findings": 1`).
const FORMAT: u64 = 1;

/// The only ratchet mode that compared changed lines against a base.
const DIFF_AND_TOTALS: &str = "diff+totals";

/// What `ratchet` notes when `git diff` threw and it fell back to the totals.
const TOTALS_ONLY: &str = "only the totals";

/// Why a ratchet is not believed (`gate-error:ratchet-degraded`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RatchetDegradation {
    /// `ratchet.mode` is not `diff+totals` (`totals`, `strict` or unknown).
    Mode,
    /// A note says only the totals were compared.
    TotalsNote,
    /// `ratchet.base` is not the merge base assay computed.
    Base,
}

impl fmt::Display for RatchetDegradation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Mode => "its mode is not diff+totals",
            Self::TotalsNote => "a note says only the totals were compared",
            Self::Base => "its base is not the merge base assay computed",
        })
    }
}

/// Why a findings document was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Refused {
    /// It is not what temper 1.3.0 writes; the detail never quotes it.
    Malformed(String),
    /// It is, but its ratchet is degraded.
    Degraded(RatchetDegradation),
}

/// temper's severity for a finding: the highest level among its rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Level {
    Unmapped,
    Consider,
    Should,
    Must,
}

/// The source that reported a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Tool {
    Clippy,
    Rustdoc,
    Scan,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckDto {
    temper_findings: u64,
    #[serde(rename = "at")]
    _at: IgnoredAny,
    #[serde(rename = "commit")]
    _commit: IgnoredAny,
    #[serde(rename = "toolchain")]
    _toolchain: IgnoredAny,
    #[serde(rename = "host")]
    _host: IgnoredAny,
    #[serde(rename = "sources")]
    _sources: IgnoredAny,
    #[serde(rename = "workspace_root")]
    _workspace_root: IgnoredAny,
    #[serde(rename = "crates")]
    _crates: IgnoredAny,
    #[serde(rename = "logs", default)]
    _logs: Option<IgnoredAny>,
    findings: Vec<Finding>,
    #[serde(rename = "errors")]
    _errors: Vec<BuildError>,
    ratchet: RatchetDto,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RatchetDto {
    ok: bool,
    mode: String,
    base: Option<String>,
    new_code: Vec<Finding>,
    increases: Vec<Increase>,
    #[serde(rename = "fixed")]
    _fixed: IgnoredAny,
    #[serde(rename = "fixed_count")]
    _fixed_count: IgnoredAny,
    #[serde(rename = "legacy_total")]
    _legacy_total: IgnoredAny,
    #[serde(rename = "baseline_total")]
    _baseline_total: IgnoredAny,
    raised: Vec<Raised>,
    errors: Vec<BuildError>,
    notes: Vec<String>,
}

/// One finding of temper's collection (clippy, rustdoc or the scan).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Finding {
    pub(super) tool: Tool,
    pub(super) lint: String,
    #[serde(rename = "crate")]
    pub(super) krate: Option<String>,
    pub(super) file: String,
    pub(super) line: NonZeroU64,
    #[serde(rename = "end_line", default)]
    _end_line: Option<IgnoredAny>,
    #[serde(rename = "column", default)]
    _column: Option<IgnoredAny>,
    #[serde(rename = "expansion", default)]
    _expansion: Option<IgnoredAny>,
    #[serde(rename = "message")]
    _message: IgnoredAny,
    #[serde(rename = "rules")]
    _rules: IgnoredAny,
    pub(super) severity: Level,
    /// The lint an `#[expect]` excepts, on the scan's `ask` hits.
    #[serde(default)]
    pub(super) exception: Option<String>,
}

/// A (crate, lint) count above the baseline.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Increase {
    #[serde(rename = "crate")]
    pub(super) krate: String,
    pub(super) lint: String,
    pub(super) baseline: u64,
    pub(super) current: u64,
}

/// A baseline count higher than at the merge base.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Raised {
    #[serde(rename = "crate")]
    pub(super) krate: String,
    pub(super) lint: String,
    pub(super) from: u64,
    pub(super) to: u64,
}

/// A compile error: the ratchet needs a workspace that builds.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BuildError {
    #[serde(rename = "tool")]
    _tool: IgnoredAny,
    #[serde(rename = "code")]
    _code: IgnoredAny,
    pub(super) file: Option<String>,
    pub(super) line: Option<u64>,
    #[serde(rename = "message")]
    _message: IgnoredAny,
    #[serde(rename = "target", default)]
    _target: Option<IgnoredAny>,
}

/// A believed ratchet and the collection it came with.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Checked {
    /// temper's own verdict on clippy, rustdoc and the scan's hard hits.
    pub(super) ok: bool,
    /// Every finding of the collection.
    pub(super) findings: Vec<Finding>,
    /// The findings on changed lines.
    pub(super) new_code: Vec<Finding>,
    pub(super) increases: Vec<Increase>,
    pub(super) raised: Vec<Raised>,
    pub(super) errors: Vec<BuildError>,
}

/// Reads `bytes`, temper's `--findings-out` document, and believes its ratchet only when it
/// diffed against `merge_base`.
///
/// The document is parsed once, refusing any repeated key (`assay_app::parse_json_strict`),
/// and only that parse is read.
pub(super) fn check(bytes: &[u8], merge_base: &str) -> Result<Checked, Refused> {
    let malformed = |error: serde_json::Error| Refused::Malformed(json_detail(&error));
    let value = assay_app::parse_json_strict(bytes).map_err(malformed)?;
    let dto: CheckDto = serde_json::from_value(value).map_err(malformed)?;
    if dto.temper_findings != FORMAT {
        return Err(Refused::Malformed(format!(
            "\"temper_findings\" is not {FORMAT}, the only format this adapter reads"
        )));
    }
    let ratchet = dto.ratchet;
    if ratchet.mode != DIFF_AND_TOTALS {
        return Err(Refused::Degraded(RatchetDegradation::Mode));
    }
    if ratchet.notes.iter().any(|note| note.contains(TOTALS_ONLY)) {
        return Err(Refused::Degraded(RatchetDegradation::TotalsNote));
    }
    if ratchet.base.as_deref() != Some(merge_base) {
        return Err(Refused::Degraded(RatchetDegradation::Base));
    }
    Ok(Checked {
        ok: ratchet.ok,
        findings: dto.findings,
        new_code: ratchet.new_code,
        increases: ratchet.increases,
        raised: ratchet.raised,
        errors: ratchet.errors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MERGE_BASE: &str = "5e1f0b9c2d3a4e5f60718293a4b5c6d7e8f90a1b";

    fn document(ratchet: &str) -> String {
        format!(
            r#"{{"temper_findings": 1, "at": "t", "commit": null, "toolchain": null,
                "host": null, "sources": [], "workspace_root": ".", "crates": [],
                "findings": [], "errors": [], "ratchet": {ratchet}}}"#
        )
    }

    fn ratchet(mode: &str, base: &str, notes: &str) -> String {
        document(&format!(
            r#"{{"ok": true, "mode": "{mode}", "base": {base}, "new_code": [],
                "increases": [], "fixed": [], "fixed_count": 0, "legacy_total": 0,
                "baseline_total": 0, "raised": [], "errors": [], "notes": [{notes}]}}"#
        ))
    }

    #[test]
    fn believes_a_diff_and_totals_ratchet_against_the_merge_base() {
        let text = ratchet("diff+totals", &format!("\"{MERGE_BASE}\""), "");

        let checked = check(text.as_bytes(), MERGE_BASE);

        assert_eq!(
            checked.map(|checked| checked.ok),
            Ok(true),
            "the one shape that is believed"
        );
    }

    #[test]
    fn a_note_mentioning_the_totals_in_other_words_is_not_degraded() {
        let note = "\"the baseline was recorded on Rust 1.97 and this run used 1.98\"";
        let text = ratchet("diff+totals", &format!("\"{MERGE_BASE}\""), note);

        assert_eq!(check(text.as_bytes(), MERGE_BASE).map(|c| c.ok), Ok(true));
    }

    #[test]
    fn levels_order_from_unmapped_to_must() {
        assert!(Level::Unmapped < Level::Consider && Level::Consider < Level::Should);
        assert!(
            Level::Should < Level::Must,
            "the increase severity takes the highest"
        );
    }
}
