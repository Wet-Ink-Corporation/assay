//! The verdict check's output and the summary comment (04 §6): fixed text, closed vocabularies
//! and digests `decide` computed, with every other string inside one fenced block per list, so
//! nothing a PR or a model wrote renders as Markdown.

use assay_domain::{Aeu, Conclusion, Counts, DegradedRole, PrClass, Reason, Tier};

use super::ports::EvidenceEntry;

/// The hidden marker the summary comment is found by (02 §5).
pub const SUMMARY_MARKER: &str = "<!-- assay-summary -->";

/// The most items of a list rendered; the rest are counted (H-13).
pub(super) const MAX_ITEMS: usize = 50;

/// The most characters of one item rendered; a longer item is clipped and marked (H-13).
pub(super) const MAX_ITEM_CHARS: usize = 200;

/// What the summary shows.
pub(super) struct Summary<'a> {
    pub(super) conclusion: Conclusion,
    pub(super) reasons: &'a [Reason],
    /// What the run established; `None` for a run refused before the rubric.
    pub(super) facts: Option<Facts<'a>>,
    /// Why the bundle was not persisted, when that is only a warning.
    pub(super) warning: Option<&'static str>,
}

/// What an authenticated run's decision adds to the summary.
pub(super) struct Facts<'a> {
    pub(super) tier: Tier,
    pub(super) classes: &'a [PrClass],
    pub(super) counts: Counts,
    pub(super) degraded: &'a [DegradedRole],
    pub(super) cost: Aeu,
    pub(super) reasons_digest: String,
}

/// What `output.text` projects of the ledger state.
pub(super) struct Projection<'a> {
    pub(super) decision_key: &'a str,
    pub(super) inputs_digest: Option<&'a str>,
    pub(super) evidence: Option<&'a EvidenceEntry>,
}

/// `output.title`: the conclusion and the first reason code.
pub(super) fn title(conclusion: Conclusion, reasons: &[Reason]) -> String {
    match reasons {
        [] => format!("assay: {}", conclusion.code()),
        [only] => format!("assay: {} ({})", conclusion.code(), only.code.code()),
        [first, rest @ ..] => format!(
            "assay: {} ({} and {} more)",
            conclusion.code(),
            first.code.code(),
            rest.len()
        ),
    }
}

/// `output.summary`: conclusion, tier and classes, AEU, counts, reasons, degraded roles, the
/// reasons digest and how to respond.
pub(super) fn summary(summary: &Summary<'_>) -> String {
    let mut lines = vec![
        format!("**assay: {}**", summary.conclusion.code()),
        String::new(),
    ];
    if let Some(facts) = &summary.facts {
        lines.extend(header(facts));
    }
    lines.push("Reasons:".to_owned());
    let reasons = summary.reasons.iter().map(|reason| {
        let fp = reason.fp.map(|fp| format!(" ({fp})")).unwrap_or_default();
        [
            reason.code.code(),
            ": ",
            reason.detail.as_str(),
            fp.as_str(),
        ]
        .concat()
    });
    block(&mut lines, summary.reasons.len(), reasons);
    if let Some(facts) = &summary.facts {
        lines.push(String::new());
        lines.push("Degraded roles:".to_owned());
        let degraded = facts
            .degraded
            .iter()
            .map(|role| [role.role.as_str(), ": ", role.reason.as_str()].concat());
        block(&mut lines, facts.degraded.len(), degraded);
    }
    if let Some(warning) = summary.warning {
        lines.push(String::new());
        lines.push(format!(
            "**Warning:** this run's input bundle was not persisted (`{warning}`): replay and \
             decide-only cannot use it; a new run is needed for them."
        ));
    }
    if let Some(facts) = &summary.facts {
        lines.push(String::new());
        lines.push(format!(
            "Reasons digest, for `/assay override`: `{}`",
            facts.reasons_digest
        ));
    }
    lines.extend(
        [
            "",
            "How to respond (an eligible owner, then `/assay recheck`):",
            "- `/assay waive <finding-id>` waives one human-decision gate finding;",
            "- `/assay override llm|all <reasons_digest> <reason>` overrides the verdict;",
            "- `/assay dismiss <fp> <reason>` dismisses one reviewer finding.",
        ]
        .map(str::to_owned),
    );
    lines.join("\n")
}

/// The summary's first facts: tier, classes and AEU, the finding counts, then a blank line.
fn header(facts: &Facts<'_>) -> [String; 3] {
    let classes: Vec<&str> = facts.classes.iter().map(|class| class.code()).collect();
    let classes = if classes.is_empty() {
        "none".to_owned()
    } else {
        classes.join(", ")
    };
    let counts = facts.counts;
    [
        format!(
            "Tier: {} · Classes: {classes} · AEU: {}",
            facts.tier.code(),
            facts.cost
        ),
        format!(
            "Findings: {} blocker · {} major · {} minor · {} nit",
            counts.blocker, counts.major, counts.minor, counts.nit
        ),
        String::new(),
    ]
}

/// `output.text`: the ledger state the check projects.
pub(super) fn text(projection: &Projection<'_>) -> String {
    let mut lines = vec![
        "```text".to_owned(),
        format!("decision_key: {}", projection.decision_key),
    ];
    if let Some(digest) = projection.inputs_digest {
        lines.push(format!("inputs_digest: {digest}"));
    }
    match projection.evidence {
        Some(entry) => {
            let pointer = &entry.pointer;
            let record = &entry.record;
            lines.push(format!(
                "evidence pointer: run_id {}, run_attempt {}, sha256 {}",
                pointer.run_id, pointer.run_attempt, pointer.sha256
            ));
            lines.push(format!(
                "evidence record: evidence_key {}, bundle_sha256 {}, llm {}, tier {}",
                record.evidence_key,
                record.bundle_sha256,
                record.llm.code(),
                record.tier.code()
            ));
        }
        None => lines.push("evidence pointer: none".to_owned()),
    }
    lines.push("```".to_owned());
    lines.join("\n")
}

/// The summary comment: the marker, then the summary.
pub(super) fn comment(summary: &str) -> String {
    [SUMMARY_MARKER, "\n\n", summary].concat()
}

/// Appends up to [`MAX_ITEMS`] of `total` items as one fenced block whose fence is longer than
/// any backtick run inside it, each item cleaned and clipped, then how many were left out.
fn block(lines: &mut Vec<String>, total: usize, items: impl Iterator<Item = String>) {
    if total == 0 {
        lines.push("none".to_owned());
        return;
    }
    let shown: Vec<String> = items.take(MAX_ITEMS).map(|item| clean(&item)).collect();
    let longest = shown.iter().map(|item| longest_backtick_run(item)).max();
    let fence = "`".repeat(longest.unwrap_or(0).saturating_add(1).max(3));
    lines.push([fence.as_str(), "text"].concat());
    let rest = total.saturating_sub(shown.len());
    lines.extend(shown);
    lines.push(fence);
    if rest > 0 {
        lines.push(format!("and {rest} more, listed in the decision."));
    }
}

/// `item` on one line: control characters (line breaks included) become U+FFFD, and past
/// [`MAX_ITEM_CHARS`] characters it is cut and ends with `…`.
fn clean(item: &str) -> String {
    let mut chars = item.chars();
    let mut cleaned: String = chars
        .by_ref()
        .take(MAX_ITEM_CHARS)
        .map(|c| if c.is_control() { '\u{fffd}' } else { c })
        .collect();
    if chars.next().is_some() {
        cleaned.push('…');
    }
    cleaned
}

/// The length of the longest run of backticks in `text`.
fn longest_backtick_run(text: &str) -> usize {
    text.split(|c| c != '`').map(str::len).max().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{MAX_ITEM_CHARS, MAX_ITEMS, block, clean, longest_backtick_run};

    fn rendered(items: &[String]) -> Vec<String> {
        let mut lines = Vec::new();
        block(&mut lines, items.len(), items.iter().cloned());
        lines
    }

    #[test]
    fn a_list_of_the_cap_is_rendered_whole() {
        let items: Vec<String> = (0..MAX_ITEMS).map(|i| format!("item {i}")).collect();

        let lines = rendered(&items);

        assert_eq!(lines.len(), MAX_ITEMS + 2, "fence, every item, fence");
        assert_eq!(lines.last().map(String::as_str), Some("```"));
    }

    #[test]
    fn a_list_past_the_cap_counts_what_it_leaves_out() {
        let items: Vec<String> = (0..=MAX_ITEMS).map(|i| format!("item {i}")).collect();

        let lines = rendered(&items);

        assert_eq!(
            lines.len(),
            MAX_ITEMS + 3,
            "fence, the cap, fence, the count"
        );
        assert_eq!(
            lines.last().map(String::as_str),
            Some("and 1 more, listed in the decision.")
        );
    }

    #[test]
    fn an_empty_list_says_none() {
        assert_eq!(rendered(&[]), ["none"]);
    }

    #[test]
    fn an_item_of_the_cap_is_kept_whole() {
        let item = "a".repeat(MAX_ITEM_CHARS);

        assert_eq!(clean(&item), item);
    }

    #[test]
    fn an_item_past_the_cap_is_clipped_and_marked() {
        let item = "é".repeat(MAX_ITEM_CHARS + 1);

        assert_eq!(
            clean(&item),
            ["é".repeat(MAX_ITEM_CHARS), "…".to_owned()].concat()
        );
    }

    #[test]
    fn control_characters_cannot_break_out_of_the_line() {
        assert_eq!(clean("a\nb\r\u{1b}c"), "a\u{fffd}b\u{fffd}\u{fffd}c");
    }

    #[test]
    fn the_fence_is_longer_than_any_backtick_run_inside() {
        let lines = rendered(&["````\n</details>".to_owned()]);

        assert_eq!(
            lines,
            ["`````text", "````\u{fffd}</details>", "`````"],
            "a four-backtick run gets a five-backtick fence"
        );
    }

    #[test]
    fn a_short_backtick_run_keeps_the_three_backtick_fence() {
        let lines = rendered(&["`code`".to_owned()]);

        assert_eq!(lines, ["```text", "`code`", "```"]);
    }

    #[test]
    fn backtick_runs_are_measured() {
        assert_eq!(longest_backtick_run("a``b```c`"), 3);
        assert_eq!(longest_backtick_run("plain"), 0);
    }
}
