//! The findings each Z2s check reports. The failing checks report `blocker`s; vendored drift is
//! a `major` (05 T4). Z4 maps gate severities itself; the gate's pass or fail never depends on
//! them (03 §2.1). PR-controlled names go through [`plain`] and into code spans.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU64;

use assay_domain::{Class, Finding, Severity, Side};

use super::baseline::{self, Raise};
use super::exceptions::Allowlist;
use super::gate_config;
use super::head::{Analysis, Blind, ExpectAt, Why};
use super::report::{Rule, Spot, finding, plain};
use super::scan::Request;
use super::vendored::{self, Drift};

/// Step 1: a baseline count rose, a new entry appeared, or a crate was added or moved.
const RAISED: Rule = Rule {
    id: "raised",
    severity: Severity::Blocker,
    class: Class::Compliance,
};

/// Step 2: a lint exception base does not admit.
const EXCEPTION: Rule = Rule {
    id: "exception",
    severity: Severity::Blocker,
    class: Class::Compliance,
};

/// Step 2: Rust the pinned scan cannot see.
const BLIND_SPOT: Rule = Rule {
    id: "blind-spot",
    severity: Severity::Blocker,
    class: Class::Security,
};

/// Step 3: a gate-config key changed without base admitting the change.
const GATE_CONFIG: Rule = Rule {
    id: "gate-config",
    severity: Severity::Blocker,
    class: Class::Security,
};

/// T4: the vendored copy differs from the pin.
const VENDORED_DRIFT: Rule = Rule {
    id: "vendored-drift",
    severity: Severity::Major,
    class: Class::Security,
};

/// One `raised` finding per raise, at the lint's line in the head baseline when it can be
/// found there.
pub(super) fn raised(raises: &[Raise<'_>], head: Option<&str>) -> Vec<Finding> {
    raises
        .iter()
        .map(|raise| {
            let Raise {
                krate,
                lint,
                from,
                to,
            } = *raise;
            let anchors = [key("totals"), key(krate), key(lint)];
            let spot = locate(head, &anchors).unwrap_or_else(|| {
                Spot::synthesised(baseline::PATH, &format!("{krate} {lint}: {to}"))
            });
            let change = from.map_or_else(
                || format!("is a new baseline entry at {to}"),
                |from| format!("rises from {from} to {to}"),
            );
            let (lint, krate) = (plain(lint), plain(krate));
            let body = format!(
                "`{lint}` in `{krate}` {change}. The temper baseline may only go down: \
                 accepting new lint debt is a human decision (H-12), made outside this PR."
            );
            let title = format!("temper baseline raised: `{lint}` in `{krate}`");
            finding(RAISED, spot, &title, &body)
        })
        .collect()
}

/// One `raised` finding per crate the head baseline adds or maps to another directory, at its
/// line in the head baseline's `crates` when it can be found there.
pub(super) fn moved_crates(names: &[&str], head: Option<&str>) -> Vec<Finding> {
    names
        .iter()
        .map(|name| {
            let spot = locate(head, &[key("crates"), key(name)])
                .unwrap_or_else(|| Spot::synthesised(baseline::PATH, name));
            let name = plain(name);
            let body = format!(
                "The head baseline adds `{name}` to `crates` or maps it to another directory. \
                 `crates` decides which sources a crate's counts cover, so this can move \
                 accepted debt onto new code; only removing a crate is allowed in a PR."
            );
            let title = format!("temper baseline crate added or moved: `{name}`");
            finding(RAISED, spot, &title, &body)
        })
        .collect()
}

/// The first line of the head baseline `anchors` reach, in turn.
fn locate(head: Option<&str>, anchors: &[String]) -> Option<Spot> {
    let anchors: Vec<&str> = anchors.iter().map(String::as_str).collect();
    Spot::locate(baseline::PATH, Side::Right, head?, &anchors)
}

/// One `exception` finding per request base does not admit, and per `#[expect]` in the head's
/// Rust that the scan did not report.
///
/// A covered hit is admitted by its (file, lint). The attribute's own hit (`expect-attr`) is
/// admitted only when every `expect` attribute at its line names exactly one lint, its lint is
/// among them, and base admits each of them in the file: temper names only an attribute's
/// first lint, so a second would otherwise ride along.
pub(super) fn exceptions(
    requests: Vec<Request>,
    analysis: &Analysis<'_>,
    allowlist: &Allowlist,
) -> Vec<Finding> {
    let reported: BTreeSet<(&str, NonZeroU64)> = requests
        .iter()
        .filter(|request| request.attribute)
        .map(|request| (request.file.as_str(), request.line))
        .collect();
    let unreported: Vec<Finding> = analysis
        .expects
        .iter()
        .filter(|expect| !reported.contains(&(expect.file, expect.line)))
        .map(unreported)
        .collect();
    let mut found: Vec<Finding> = requests
        .into_iter()
        .filter(|request| !admitted(request, analysis, allowlist))
        .map(unadmitted)
        .collect();
    found.extend(unreported);
    found
}

/// Whether base admits `request`.
fn admitted(request: &Request, analysis: &Analysis<'_>, allowlist: &Allowlist) -> bool {
    let Some(lint) = request.lint.as_deref() else {
        return false;
    };
    if !allowlist.admits_exception(&request.file, lint) {
        return false;
    }
    if !request.attribute {
        return true;
    }
    let mut lints = analysis
        .expects
        .iter()
        .filter(|expect| expect.file == request.file && expect.line == request.line)
        .map(|expect| match expect.lints.as_slice() {
            [Some(only)] => Some(only.as_str()),
            _ => None,
        })
        .peekable();
    let mut named = false;
    let mut all_admitted = lints.peek().is_some();
    for only in lints {
        named |= only == Some(lint);
        all_admitted &= only.is_some_and(|only| allowlist.admits_exception(&request.file, only));
    }
    named && all_admitted
}

fn unadmitted(request: Request) -> Finding {
    let lint = request
        .lint
        .as_deref()
        .map_or_else(|| "?".to_owned(), plain);
    let fallback = format!("#[expect({lint})]");
    let body = format!(
        "temper reports an exception request for `{lint}` here. A lint exception is accepted \
         only when base `.assay/exceptions.toml` names this file and lint in an `[[exception]]` \
         entry, and its `#[expect]` names that one lint (H-05)."
    );
    let title = format!("unapproved lint exception: `{lint}`");
    let spot = Spot::at(
        request.file,
        request.line,
        request.text.as_deref(),
        &fallback,
    );
    finding(EXCEPTION, spot, &title, &body)
}

fn unreported(expect: &ExpectAt<'_>) -> Finding {
    let spot = Spot::at(expect.file.to_owned(), expect.line, None, "#[expect(..)]");
    let body = "This `#[expect]` was not reported by the pinned temper scan, so base cannot have \
                admitted it. Every lint exception must be one the scan reports and base \
                `.assay/exceptions.toml` admits (H-05).";
    finding(
        EXCEPTION,
        spot,
        "lint exception the scan did not report",
        body,
    )
}

/// One `blind-spot` finding per path or attribute the pinned scan cannot see.
pub(super) fn blind(spots: &[&Blind<'_>]) -> Vec<Finding> {
    spots
        .iter()
        .map(|spot| {
            let path = plain(spot.path);
            let (title, body) = wording(spot.why);
            let fallback = format!("{path}: {title}");
            let located = Spot::at(
                spot.path.to_owned(),
                spot.line,
                spot.quote.as_deref(),
                &fallback,
            );
            let body = format!("`{path}`: {body}");
            finding(BLIND_SPOT, located, &format!("{title}: `{path}`"), &body)
        })
        .collect()
}

/// A blind spot's title and the body that explains it.
const fn wording(why: Why) -> (&'static str, &'static str) {
    match why {
        Why::SkippedDirectory => (
            "Rust source where the temper scan does not look",
            "This `.rs` file sits under `target`, `.git`, `node_modules` or `.temper`, \
                     which the temper scan skips, yet a `#[path]`, `include!` or build script \
                     could compile it.",
        ),
        Why::Submodule => (
            "submodule the temper scan does not read",
            "A submodule's files are not in this tree, so the scan cannot see Rust \
                     that a recursive checkout would compile.",
        ),
        Why::Symlink => (
            "symlink the temper scan cannot follow safely",
            "This symlink is a Rust file, which rustc and the scan need not read as the \
                     same module, or it does not resolve, inside the tree and through real \
                     directories only, to a regular file. The scan does not walk symlinked \
                     directories, but rustc follows them. Commit the file itself.",
        ),
        Why::Extension => (
            "Rust source the temper scan does not match",
            "This Rust file's extension is not exactly `.rs`, so the temper scan \
                     skips it, yet a `#[path]` or a Cargo target path could compile it. Name \
                     it `.rs`.",
        ),
        Why::Attribute => (
            "attribute the temper scan does not read",
            "A `cfg_attr` carrying `allow`, `expect`, `path` or a macro fragment, a \
                     `#[path]` attribute, a lint attribute without its `(..)` list, an \
                     attribute named by `r#`, a `#` that does not start an attribute, a macro \
                     fragment that can assemble one or name a macro, or a renamed `include`: \
                     the scan sees neither the suppression nor the file it compiles. Write \
                     the attribute plainly, and keep modules at their default paths.",
        ),
        Why::Reference => (
            "file the temper scan cannot pin down",
            "An `include!`, whose text rustc reads in a context the scan does not model, \
                     or a `mod` declaration that does not name, from this file, a `.rs` file \
                     of the tree the scan reads wherever rustc can look for it: a build \
                     script can write the file rustc compiles after the scan ran. Declare \
                     modules at the top level of a file, outside brackets and macro bodies, \
                     with their files in the tree, and do not `include!` Rust.",
        ),
        Why::Unreported => (
            "`allow` the temper scan did not report",
            "This `allow` attribute is one the temper scan should report at this line, \
                     and it did not, so its baseline does not count it either. Remove the \
                     `allow`, or write it plainly on one line.",
        ),
        Why::Unreadable => (
            "Rust source assay could not read",
            "A literal or comment in this file never ends, its code holds a character \
                     outside ASCII or a token whose boundary depends on the edition (`cr\"`, \
                     `'r#a'`, or a suffix, lifetime or raw name ending in `r` against a `#` or \
                     `\"`), it starts with a shebang line or frontmatter, or its bytes were not \
                     read, so its attributes cannot be checked.",
        ),
    }
}

/// One `gate-config` finding per drifted key, at its head line, or at its base line when head
/// removed it.
pub(super) fn gate_config(keys: &[&str], base: Option<&str>, head: Option<&str>) -> Vec<Finding> {
    keys.iter()
        .map(|name| {
            let anchor = key(name);
            let at = |side, text: Option<&str>| {
                text.and_then(|text| Spot::locate(gate_config::PATH, side, text, &[&anchor]))
            };
            let spot = at(Side::Right, head)
                .or_else(|| at(Side::Left, base))
                .unwrap_or_else(|| Spot::synthesised(gate_config::PATH, &anchor));
            let body = format!(
                "`{name}` in `.claude/temper.json` differs from base. It steers what temper \
                 checks, so a change is accepted only when base `.assay/exceptions.toml` names \
                 the key, its exact base value and its exact new value in a `[[gate_config]]` \
                 entry."
            );
            let title = format!("temper gate config changed: `{name}`");
            finding(GATE_CONFIG, spot, &title, &body)
        })
        .collect()
}

/// One `vendored-drift` finding naming the first drifted path, or none.
pub(super) fn drift(drifted: &BTreeMap<Cow<'_, str>, Drift>, pin: &str) -> Option<Finding> {
    let (path, first) = drifted.first_key_value()?;
    let shown = plain(path);
    let spot = Spot::synthesised(path, &format!("{shown} {}", first.describe()));
    let body = format!(
        "{count} path(s) under `{dir}/` differ from the pinned temper `{pin}`; the first, \
         `{shown}`, {what}. assay runs only the pinned temper and ignores this copy, but CI and \
         the hooks run it. Re-vendor the pinned version with `temper setup --vendor-only`.",
        count = drifted.len(),
        dir = vendored::DIR,
        what = first.describe(),
    );
    let title = "the vendored temper drifted from the pin";
    Some(finding(VENDORED_DRIFT, spot, title, &body))
}

/// `name` as a JSON object key is written, `"name":` (a name needing escapes is not found, and
/// the finding falls back to a synthesised quote).
fn key(name: &str) -> String {
    format!("\"{name}\":")
}
