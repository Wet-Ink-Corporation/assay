//! The ruleset checks: the verdict check required on every protected base (R3-03, R3-01), the
//! trust paths restricted (05 §7), and the ledger's branches restricted to its App (R3-20).
//!
//! Only an active ruleset with no bypass actor counts for the first two: GitHub has no
//! individual-user bypass actor, so "only the break-glass account" cannot be shown from a
//! ruleset alone and a non-empty list fails (fail-closed, wi-b5affa).

use std::collections::BTreeSet;

use super::port::{
    AppliedRule, BypassActor, Enforcement, PrerequisiteForge, Rule, Ruleset, RulesetId,
    RulesetTarget,
};
use super::report::PrerequisiteFailure;
use super::{Judged, Scene};
use crate::publish::CheckName;
use crate::status::AppId;

/// The trust paths a push ruleset must restrict, as `file_path_restriction` patterns.
pub(super) const TRUST_PATHS: [&str; 5] = [
    ".github/workflows/**",
    ".assay/**",
    ".claude/**",
    "CODEOWNERS",
    ".gitattributes",
];

/// The `ref_name` patterns that cover every branch.
const ALL_BRANCHES: [&str; 2] = ["~ALL", "refs/heads/**"];

/// The default branch and every other protected base require their verdict check from the
/// App, with branches up to date, through a ruleset without bypass actors.
pub(super) fn verdict<F: PrerequisiteForge>(scene: &Scene<'_, F>) -> Judged<F::Error> {
    let default = &scene.base.default_branch;
    let bases = &scene.base.config.assay().protected_bases;
    if !bases.contains(default) {
        return Err(PrerequisiteFailure::DefaultNotProtected {
            branch: default.clone(),
        });
    }
    let verdicts = Verdicts {
        rulesets: scene.rulesets.get()?,
        app: scene.apps.verdict,
    };
    for base in bases {
        let rules = scene
            .repository
            .branch_rules(base)
            .map_err(PrerequisiteFailure::Unreadable)?;
        verdicts.judge(base, CheckName::for_base(base, default), &rules)?;
    }
    Ok(())
}

/// What a base's verdict rules are judged against: the repository's rulesets and the App.
struct Verdicts<'a> {
    rulesets: &'a [Ruleset],
    app: AppId,
}

impl Verdicts<'_> {
    /// `base` holds when one of its `rules` requires `check` from the App, up to date, through
    /// a listed ruleset without bypass actors; otherwise the first such rule's problem, or
    /// that no rule requires it.
    fn judge<E>(&self, base: &str, check: CheckName, rules: &[AppliedRule]) -> Judged<E> {
        let mut problem = None;
        for applied in rules {
            let Rule::RequiredStatusChecks(required) = &applied.rule else {
                continue;
            };
            let names_it = required.checks.iter().any(|candidate| {
                candidate.context == check.as_str() && candidate.integration == Some(self.app)
            });
            if !names_it {
                continue;
            }
            let found = if required.strict {
                unbypassable(self.rulesets, applied.ruleset)
            } else {
                Some(PrerequisiteFailure::NotUpToDate {
                    base: base.to_owned(),
                    ruleset: applied.ruleset,
                })
            };
            match found {
                None => return Ok(()),
                Some(failure) if problem.is_none() => problem = Some(failure),
                Some(_) => {}
            }
        }
        Err(
            problem.unwrap_or_else(|| PrerequisiteFailure::CheckNotRequired {
                base: base.to_owned(),
                check,
            }),
        )
    }
}

/// `None` when ruleset `id` is listed, active and has no bypass actor; why not otherwise.
fn unbypassable<E>(rulesets: &[Ruleset], id: RulesetId) -> Option<PrerequisiteFailure<E>> {
    let Some(ruleset) = rulesets.iter().find(|ruleset| ruleset.id == id) else {
        return Some(PrerequisiteFailure::UnknownRuleset { ruleset: id });
    };
    // `rules/branches` lists only rules in force; the ruleset's own enforcement is checked
    // too, so neither answer alone is trusted.
    if ruleset.enforcement != Enforcement::Active {
        return Some(PrerequisiteFailure::NotActive { ruleset: id });
    }
    // The report owns its failures; the ruleset stays the caller's.
    ruleset
        .bypass
        .first()
        .cloned()
        .map(|actor| PrerequisiteFailure::Bypass { ruleset: id, actor })
}

/// Every trust path is restricted by an active push ruleset without bypass actors.
pub(super) fn paths<F: PrerequisiteForge>(scene: &Scene<'_, F>) -> Judged<F::Error> {
    let restricted: BTreeSet<&str> = scene
        .rulesets
        .get()?
        .iter()
        .filter(|ruleset| {
            ruleset.target == RulesetTarget::Push
                && ruleset.enforcement == Enforcement::Active
                && ruleset.bypass.is_empty()
        })
        .flat_map(|ruleset| &ruleset.rules)
        .filter_map(|rule| match rule {
            Rule::FilePathRestriction(paths) => Some(paths),
            Rule::RequiredStatusChecks(_)
            | Rule::Creation
            | Rule::Update
            | Rule::Deletion
            | Rule::Other(_) => None,
        })
        .flatten()
        .map(String::as_str)
        .collect();
    match TRUST_PATHS
        .into_iter()
        .find(|path| !restricted.contains(path))
    {
        Some(path) => Err(PrerequisiteFailure::PathUnrestricted { path }),
        None => Ok(()),
    }
}

/// An active branch ruleset of the ledger covers every branch, restricts creating, updating
/// and deleting them, and lets only the ledger App bypass it.
pub(super) fn ledger<F: PrerequisiteForge>(scene: &Scene<'_, F>) -> Judged<F::Error> {
    let ledger = scene.ledger.ok_or(PrerequisiteFailure::NoLedger)?;
    let rulesets = ledger.rulesets().map_err(PrerequisiteFailure::Unreadable)?;
    let only_app = [BypassActor::Integration(scene.apps.ledger)];
    let restricts = |ruleset: &Ruleset| {
        let conditions = &ruleset.conditions;
        ruleset.target == RulesetTarget::Branch
            && ruleset.enforcement == Enforcement::Active
            && !conditions.other
            && conditions.excluded.is_empty()
            && conditions
                .included
                .iter()
                .any(|pattern| ALL_BRANCHES.contains(&pattern.as_str()))
            && [Rule::Creation, Rule::Update, Rule::Deletion]
                .iter()
                .all(|rule| ruleset.rules.contains(rule))
            && ruleset.bypass == only_app
    };
    if rulesets.iter().any(restricts) {
        Ok(())
    } else {
        Err(PrerequisiteFailure::LedgerUnrestricted)
    }
}
