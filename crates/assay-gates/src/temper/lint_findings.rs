//! The `gates-lint` findings (`docs/architecture/03-contracts.md` §2.1 steps 5-6).
//!
//! Failures: every `raised` entry, and every `exception` hit base `.assay/exceptions.toml` does
//! not admit by file and lint. They can only add to temper's verdict, and their absence proves
//! nothing (Z2s checks both from the trees). Display: the findings on changed lines, each count
//! above the baseline, and each located compile error, with temper's severity mapped
//! `must` to `blocker`, `should` to `major`, `consider` and `unmapped` to `minor`. Display never
//! decides the gate. At most [`MAX_REPORTED`] findings are reported, failures first.

use std::collections::HashMap;
use std::num::NonZeroU64;

use assay_domain::{Class, Finding, Severity};

use super::baseline::{self, Raise};
use super::evidence::Lines;
use super::exceptions::Allowlist;
use super::findings;
use super::ratchet::{self, BuildError, Checked, Increase, Level, Tool};
use super::report::{Rule, Spot, finding, plain};
use super::scan::Request;

/// Most findings in one `gates-lint` document, so it stays far below the 8 MiB contract cap.
pub(super) const MAX_REPORTED: usize = 1_000;

/// The scan key temper reports for the `#[expect]` attribute itself.
const EXPECT_ATTR: &str = "scan:expect-attr";

/// Rule ids `decide` may waive (`rubric/v1.toml`); a lint named like one is prefixed, so no
/// clippy or rustdoc finding can ever be waived.
const WAIVABLE: [&str; 5] = [
    "raised",
    "exception",
    "gate-config",
    "test-inventory",
    "manifest-switch",
];

/// Rule of a compile error.
const COMPILE_ERROR: &str = "compile-error";

/// The `raised` and unadmitted `exception` entries, which fail the gate.
pub(super) struct Failures {
    /// Whether there is any: decided over every entry, never over the reported ones.
    pub(super) failing: bool,
    /// At most [`MAX_REPORTED`] of their findings, `raised` first.
    pub(super) findings: Vec<Finding>,
}

/// The `raised` and unadmitted `exception` entries of `checked`.
///
/// A `raised` finding's spot is synthesised at the baseline rather than located in the head
/// baseline, which the PR controls: locating each would scan the file once per entry.
pub(super) fn failures(checked: &Checked, allowlist: &Allowlist) -> Failures {
    let mut unadmitted = checked
        .findings
        .iter()
        .filter_map(|hit| Some((hit, hit.exception.as_deref()?)))
        .filter(|(hit, lint)| !allowlist.admits_exception(&hit.file, lint))
        .peekable();
    let failing = !checked.raised.is_empty() || unadmitted.peek().is_some();
    let raises: Vec<Raise<'_>> = checked
        .raised
        .iter()
        .take(MAX_REPORTED)
        .map(|raised| Raise {
            krate: &raised.krate,
            lint: &raised.lint,
            // temper counts an entry base lacks from 0.
            from: (raised.from > 0).then_some(raised.from),
            to: raised.to,
        })
        .collect();
    let mut found = findings::raised(&raises, None);
    let room = MAX_REPORTED.saturating_sub(found.len());
    found.extend(unadmitted.take(room).map(|(hit, lint)| {
        // The request, and then the document, own the path and the lint.
        findings::unadmitted(Request {
            file: hit.file.clone(),
            attribute: hit.lint == EXPECT_ATTR,
            line: hit.line,
            lint: Some(lint.to_owned()),
            text: None,
        })
    }));
    Failures {
        failing,
        findings: found,
    }
}

/// One finding to display, before its evidence is read.
#[derive(Debug, Clone, Copy)]
pub(super) enum Shown<'c> {
    /// A finding on a changed line.
    Code(&'c ratchet::Finding),
    /// A count above the baseline, with the highest severity temper gave that lint.
    Increase(&'c Increase, Level),
    /// A compile error at a file and line.
    Error(&'c str, NonZeroU64),
}

impl<'c> Shown<'c> {
    /// The file and line whose text is its evidence, if any.
    pub(super) const fn cites(&self) -> Option<(&'c str, NonZeroU64)> {
        match *self {
            Self::Code(code) => Some((code.file.as_str(), code.line)),
            Self::Increase(..) => None,
            Self::Error(file, line) => Some((file, line)),
        }
    }
}

/// What to display after `failing` failures, in order, within [`MAX_REPORTED`] in all.
pub(super) fn shown(checked: &Checked, failing: usize) -> Vec<Shown<'_>> {
    let mut highest: HashMap<&str, Level> = HashMap::new();
    for hit in &checked.findings {
        let level = highest.entry(hit.lint.as_str()).or_insert(hit.severity);
        *level = (*level).max(hit.severity);
    }
    let increases = checked.increases.iter().map(|increase| {
        let level = highest
            .get(increase.lint.as_str())
            .copied()
            .unwrap_or(Level::Unmapped);
        Shown::Increase(increase, level)
    });
    let errors = checked.errors.iter().filter_map(located);
    checked
        .new_code
        .iter()
        .map(Shown::Code)
        .chain(increases)
        .chain(errors)
        .take(MAX_REPORTED.saturating_sub(failing))
        .collect()
}

/// A compile error with a file and a line.
fn located(error: &BuildError) -> Option<Shown<'_>> {
    let line = NonZeroU64::new(error.line?)?;
    Some(Shown::Error(error.file.as_deref()?, line))
}

/// The finding for `shown`, quoting `lines`.
pub(super) fn display(shown: Shown<'_>, lines: &Lines<'_>) -> Finding {
    match shown {
        Shown::Code(code) => changed_line(code, lines),
        Shown::Increase(increase, level) => increased(increase, level),
        Shown::Error(file, line) => {
            let spot = lines.spot(file, line, COMPILE_ERROR);
            let body = "The pinned temper `baseline --check` reports a compile error here; the \
                        ratchet needs a workspace that builds.";
            let rule = rule(Class::Logic, Severity::Blocker);
            let error = finding(rule, spot, "the workspace does not build here", body);
            under(error, COMPILE_ERROR)
        }
    }
}

/// A finding on a line this PR changed.
fn changed_line(code: &ratchet::Finding, lines: &Lines<'_>) -> Finding {
    let lint = plain(&code.lint);
    let level = level_name(code.severity);
    let spot = lines.spot(&code.file, code.line, &lint);
    let (tool, class) = match code.tool {
        Tool::Clippy => ("clippy", Class::Compliance),
        Tool::Rustdoc => ("rustdoc", Class::Docs),
        Tool::Scan => ("temper scan", Class::Compliance),
    };
    let body = format!(
        "The pinned temper `baseline --check` reports `{lint}` (temper severity `{level}`) on a \
         line this PR changed. The ratchet fails on any finding in changed code (H-15)."
    );
    let rule = rule(class, severity(code.severity));
    under(
        finding(rule, spot, &format!("{tool}: `{lint}`"), &body),
        &code.lint,
    )
}

/// A (crate, lint) count above the baseline, at the baseline.
fn increased(increase: &Increase, level: Level) -> Finding {
    let (lint, krate) = (plain(&increase.lint), plain(&increase.krate));
    let (was, now) = (increase.baseline, increase.current);
    let spot = Spot::synthesised(baseline::PATH, &format!("{krate} {lint}: {was} -> {now}"));
    let body = format!(
        "`{lint}` in `{krate}` has {now} findings, above the baseline's {was}. The ratchet \
         fails when a count rises, even on lines this PR did not change."
    );
    let class = if increase.lint.starts_with("rustdoc::") {
        Class::Docs
    } else {
        Class::Compliance
    };
    let title = format!("temper count rose: `{lint}` in `{krate}`");
    under(
        finding(rule(class, severity(level)), spot, &title, &body),
        &increase.lint,
    )
}

/// A display rule of `class` and `severity`; its id is replaced by [`under`].
const fn rule(class: Class, severity: Severity) -> Rule {
    Rule {
        id: COMPILE_ERROR,
        severity,
        class,
    }
}

/// `shown` under the rule `lint`, made safe and never one `decide` can waive.
fn under(mut shown: Finding, lint: &str) -> Finding {
    let id = plain(lint);
    shown.rule = Some(if WAIVABLE.contains(&id.as_str()) {
        format!("lint:{id}")
    } else {
        id
    });
    shown
}

/// 03 §2.1 step 6, for display only.
const fn severity(level: Level) -> Severity {
    match level {
        Level::Must => Severity::Blocker,
        Level::Should => Severity::Major,
        Level::Consider | Level::Unmapped => Severity::Minor,
    }
}

const fn level_name(level: Level) -> &'static str {
    match level {
        Level::Must => "must",
        Level::Should => "should",
        Level::Consider => "consider",
        Level::Unmapped => "unmapped",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MERGE_BASE: &str = "5e1f0b9c2d3a4e5f60718293a4b5c6d7e8f90a1b";

    /// A believed ratchet with `count` findings on changed lines.
    fn with_new_code(count: usize) -> Checked {
        let hit = serde_json::json!({
            "tool": "clippy", "lint": "clippy::x", "crate": "c", "file": "f.rs", "line": 1,
            "message": "m", "rules": [], "severity": "must",
        });
        let document = serde_json::json!({
            "temper_findings": 1, "at": "t", "commit": null, "toolchain": null, "host": null,
            "sources": [], "workspace_root": ".", "crates": [], "findings": [], "errors": [],
            "ratchet": {
                "ok": false, "mode": "diff+totals", "base": MERGE_BASE,
                "new_code": vec![hit; count], "increases": [], "fixed": [], "fixed_count": 0,
                "legacy_total": 0, "baseline_total": 0, "raised": [], "errors": [], "notes": [],
            },
        });
        ratchet::check(document.to_string().as_bytes(), MERGE_BASE).expect("a believed ratchet")
    }

    #[test]
    fn shows_every_finding_up_to_the_cap() {
        let checked = with_new_code(MAX_REPORTED);

        assert_eq!(shown(&checked, 0).len(), MAX_REPORTED);
    }

    #[test]
    fn shows_none_past_the_cap_and_leaves_room_for_the_failures() {
        let checked = with_new_code(MAX_REPORTED + 1);

        let lengths = (shown(&checked, 0).len(), shown(&checked, 3).len());

        assert_eq!(lengths, (MAX_REPORTED, MAX_REPORTED - 3));
    }

    #[test]
    fn a_lint_named_like_a_waivable_rule_is_prefixed() {
        let rule = rule(Class::Compliance, Severity::Minor);
        let plain = finding(rule, Spot::synthesised("f.rs", "x"), "t", "b");

        let shown = under(plain, "raised");

        assert_eq!(shown.rule.as_deref(), Some("lint:raised"));
    }

    #[test]
    fn severities_map_as_03_says() {
        let mapped = [Level::Must, Level::Should, Level::Consider, Level::Unmapped].map(severity);

        assert_eq!(
            mapped,
            [
                Severity::Blocker,
                Severity::Major,
                Severity::Minor,
                Severity::Minor
            ]
        );
    }
}
