//! The findings of step 4 (`docs/architecture/03-contracts.md` §2.1). `test-inventory` and
//! `manifest-switch` are human decisions a waiver can clear (rubric v1 `[[waivable]]`); a
//! weakened lints table, a member that does not inherit the workspace lints and a manifest
//! that does not parse are not. PR-controlled text goes through [`plain`] into code spans.

use assay_domain::{Class, Finding, Severity, Side};

use super::exceptions::Allowlist;
use super::manifests::{ManifestChecks, SwitchChange};
use super::report::{Rule, Spot, finding, plain};
use super::statics::{InventoryChange, Loss};

/// Step 4a: tests lost, an ignore added, or a test or module put under a new condition.
const TEST_INVENTORY: Rule = Rule {
    id: "test-inventory",
    severity: Severity::Blocker,
    class: Class::Compliance,
};

/// One finding per file whose test inventory lost something, unless base admits that file's
/// exact change. A file only base holds is reported on the base side.
pub(super) fn inventory(changes: &[InventoryChange<'_>], allowlist: &Allowlist) -> Vec<Finding> {
    changes
        .iter()
        .filter(|change| !allowlist.admits_inventory(change.path, change.base, change.head))
        .map(|change| {
            let losses: Vec<String> = change.losses.iter().map(describe).collect();
            let path = plain(change.path);
            let side = if change.head.is_some() {
                Side::Right
            } else {
                Side::Left
            };
            let spot = Spot::synthesised_on(change.path, side, &losses.join("; "));
            let body = format!(
                "`{path}`: {}. Removing, ignoring or switching off tests is a human decision \
                 (H-12): base `.assay/exceptions.toml` admits it only with a \
                 `[[test_inventory]]` entry naming this path and its exact base blob ({}) and \
                 head blob ({}).",
                losses.join("; "),
                blob(change.base),
                blob(change.head),
            );
            let title = format!("test inventory lost: `{path}`");
            finding(TEST_INVENTORY, spot, &title, &body)
        })
        .collect()
}

/// A loss in words; PR-controlled text is made plain.
fn describe(loss: &Loss) -> String {
    match loss {
        Loss::Tests { from, to } => format!("tests {from} -> {to}"),
        Loss::Ignores { from, to } => format!("ignores {from} -> {to}"),
        Loss::Gate(text) => format!("new condition `{}`", plain(text)),
        Loss::Module(name) => format!("module `{}` no longer declared", plain(name)),
        Loss::Unreadable => "its base text could not be counted".to_owned(),
    }
}

/// Step 4b: a manifest switch changed.
const MANIFEST_SWITCH: Rule = Rule {
    id: "manifest-switch",
    severity: Severity::Blocker,
    class: Class::Compliance,
};

/// Step 4c: a lints table weakened. Never waivable.
const LINTS_TABLE: Rule = Rule {
    id: "lints-table",
    severity: Severity::Blocker,
    class: Class::Compliance,
};

/// Step 4d: a package does not inherit the workspace lints. Never waivable.
const LINT_INHERITANCE: Rule = Rule {
    id: "lint-inheritance",
    severity: Severity::Blocker,
    class: Class::Compliance,
};

/// Steps 4b-4d: a manifest that does not parse, so nothing in it can be compared. Never
/// waivable.
const MANIFEST_UNREADABLE: Rule = Rule {
    id: "manifest-unreadable",
    severity: Severity::Blocker,
    class: Class::Security,
};

/// The findings of steps 4b-4d: unreadable manifests, then switches base does not admit, then
/// weakened lints tables, then packages that do not inherit, each by path.
pub(super) fn manifests(checks: &ManifestChecks<'_>, allowlist: &Allowlist) -> Vec<Finding> {
    let side = |at_head: bool| if at_head { Side::Right } else { Side::Left };
    let unreadable = checks.unreadable.iter().map(|&(path, at_head)| {
        let spot = Spot::synthesised_on(path, side(at_head), "not a TOML manifest");
        let body = format!(
            "`{}` is not UTF-8 TOML, so its switches, lints and inheritance cannot be \
             compared. Cargo refuses it too.",
            plain(path)
        );
        let title = format!("manifest that does not parse: `{}`", plain(path));
        finding(MANIFEST_UNREADABLE, spot, &title, &body)
    });
    let switches = checks
        .switches
        .iter()
        .filter(|change| !allowlist.admits_switch(change.path, change.base, change.head))
        .map(switch);
    let weakened = checks.weakened.iter().map(|(path, at_head, what)| {
        let spot = Spot::synthesised_on(path, side(*at_head), &what.join("; "));
        let body = format!(
            "`{}`: {}. A lint table may only get stricter in a PR: lowering a level, removing \
             a lint, changing a priority or adding a lint below `deny` is a human decision made \
             outside this gate (H-05, H-12).",
            plain(path),
            what.iter()
                .map(|text| plain(text))
                .collect::<Vec<_>>()
                .join("; "),
        );
        let title = format!("lints table weakened: `{}`", plain(path));
        finding(LINTS_TABLE, spot, &title, &body)
    });
    let uninherited = checks.uninherited.iter().map(|path| {
        let spot = Spot::synthesised(path, "no [lints] workspace = true");
        let body = format!(
            "`{}` declares a package without `[lints] workspace = true`, so the workspace \
             lints do not apply to it. Every package outside a directory the workspace \
             excludes must inherit them.",
            plain(path)
        );
        let title = format!(
            "package does not inherit the workspace lints: `{}`",
            plain(path)
        );
        finding(LINT_INHERITANCE, spot, &title, &body)
    });
    unreadable
        .chain(switches)
        .chain(weakened)
        .chain(uninherited)
        .collect()
}

/// One `manifest-switch` finding.
fn switch(change: &SwitchChange<'_>) -> Finding {
    let path = plain(change.path);
    let names = change.switches.join(", ");
    let side = if change.head.is_some() {
        Side::Right
    } else {
        Side::Left
    };
    let spot = Spot::synthesised_on(change.path, side, &format!("changed: {names}"));
    let body = format!(
        "`{path}` changes {names}. These decide which tests run and what the code compiles \
         against, so a change is a human decision (H-12, H-16): base `.assay/exceptions.toml` \
         admits it only with a `[[manifest_switch]]` entry naming this path and its exact base \
         blob ({}) and head blob ({}).",
        blob(change.base),
        blob(change.head),
    );
    let title = format!("manifest switch changed: `{path}`");
    finding(MANIFEST_SWITCH, spot, &title, &body)
}

/// A blob id as the body shows it: the id is git's hex, or `absent`.
fn blob(id: Option<&str>) -> String {
    id.map_or_else(|| "absent".to_owned(), |id| format!("`{}`", plain(id)))
}
