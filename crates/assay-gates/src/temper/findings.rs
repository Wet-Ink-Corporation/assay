//! The findings each Z2s check reports. The three failing checks report `blocker`s; vendored
//! drift is a `major` (05 T4). Z4 maps gate severities itself; the gate's pass or fail never
//! depends on them (03 §2.1).

use assay_domain::{Class, Finding, Severity, Side};

use super::baseline::{self, Raise};
use super::exceptions::Allowlist;
use super::gate_config;
use super::report::{Rule, Spot, finding, plain};
use super::scan::Request;
use super::vendored::{self, Drift};

/// Step 1: a baseline count rose, or a new entry appeared.
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

/// Step 3: a gate-config key changed without base admitting its value.
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
            let anchors = anchors.each_ref().map(String::as_str);
            let spot = head
                .and_then(|text| Spot::locate(baseline::PATH, Side::Right, text, &anchors))
                .unwrap_or_else(|| {
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
            let title = format!("temper baseline raised: {lint} in {krate}");
            finding(RAISED, spot, &title, &body)
        })
        .collect()
}

/// One `exception` finding per request that base does not admit.
pub(super) fn exceptions(requests: Vec<Request>, allowlist: &Allowlist) -> Vec<Finding> {
    requests
        .into_iter()
        .filter(|request| {
            request
                .lint
                .as_deref()
                .is_none_or(|lint| !allowlist.admits_exception(&request.file, lint))
        })
        .map(|request| {
            let lint = request
                .lint
                .as_deref()
                .map_or_else(|| "?".to_owned(), plain);
            let fallback = format!("#[expect({lint})]");
            let body = format!(
                "temper reports an exception request for `{lint}` here. A lint exception is \
                 accepted only when base `.assay/exceptions.toml` names this file and lint in an \
                 `[[exception]]` entry (H-05)."
            );
            let title = format!("unapproved lint exception: {lint}");
            let spot = Spot::at(
                request.file,
                request.line,
                request.text.as_deref(),
                &fallback,
            );
            finding(EXCEPTION, spot, &title, &body)
        })
        .collect()
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
                 the key and its exact new value in a `[[gate_config]]` entry."
            );
            let title = format!("temper gate config changed: {name}");
            finding(GATE_CONFIG, spot, &title, &body)
        })
        .collect()
}

/// One `vendored-drift` finding naming the first drifted path, or none.
pub(super) fn drift(drifted: &[(String, Drift)], pin: &str) -> Option<Finding> {
    let (path, first) = drifted.first()?;
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
