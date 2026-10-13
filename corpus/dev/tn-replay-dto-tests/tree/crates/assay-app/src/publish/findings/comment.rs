//! An inline comment's body: fixed text and closed vocabularies around model prose made inert
//! by [`prose`], publisher-built permalinks and fences, the hidden fingerprint marker and the
//! feedback footer (04 §6).

use assay_domain::{Fingerprint, Severity};

use crate::config::CommitId;
use crate::publish::sanitise::{Info, Repository, code_block, permalink, prose};
use crate::validation::ValidFinding;

/// What the hidden marker line starts with; the fingerprint and [`MARKER_END`] follow.
const MARKER_START: &str = "<!-- assay-finding ";

/// What the hidden marker line ends with.
const MARKER_END: &str = " -->";

/// The most characters of a title shown (the findings schema's cap).
const MAX_TITLE_CHARS: usize = 100;

/// The most characters of a body shown (the findings schema's cap).
const MAX_BODY_CHARS: usize = 1200;

/// The most characters of a trigger or expected behaviour shown (the findings schema's cap).
const MAX_CLAIM_CHARS: usize = 300;

/// The most characters of a suggested fix shown (the findings schema's cap).
const MAX_FIX_CHARS: usize = 600;

/// The most characters of a rule id shown.
const MAX_RULE_CHARS: usize = 100;

/// The body of the inline comment for `found`, whose permalinks point into `repository` at
/// `head`: the marker line, a header of closed vocabularies, the title, body and labelled
/// fields as inert prose, the caller's permalink and code, the evidence, a note on a similar
/// dismissal, and the footer inviting a reaction. Each free-text field is capped before it is
/// escaped, so the body's length is bounded whatever the finding holds.
pub(super) fn body(found: &ValidFinding, repository: Repository<'_>, head: &CommitId) -> String {
    let finding = &found.finding;
    let fp = found.fingerprint;
    let mut header = format!(
        "{} **{}** · {} · {} · confidence {}",
        finding.marker(),
        finding.severity.code(),
        finding.class.code(),
        finding.kind.code(),
        finding
            .confidence
            .map_or("unstated", |confidence| confidence.code()),
    );
    if finding.pre_existing {
        header.push_str(" · pre-existing");
    }
    let mut parts = vec![
        [MARKER_START, &fp.to_string(), MARKER_END, "\n", &header].concat(),
        format!("**{}**", prose(&finding.title, MAX_TITLE_CHARS)),
        prose(&finding.body, MAX_BODY_CHARS),
    ];
    let labelled = [
        ("Trigger", finding.trigger.as_deref(), MAX_CLAIM_CHARS),
        (
            "Expected behaviour",
            finding.expected_behavior.as_deref(),
            MAX_CLAIM_CHARS,
        ),
        ("Rule", finding.rule.as_deref(), MAX_RULE_CHARS),
    ];
    for (label, text, cap) in labelled {
        if let Some(text) = text {
            parts.push(format!("**{label}:** {}", prose(text, cap)));
        }
    }
    if let Some(caller) = &finding.caller {
        parts.push(format!(
            "**Caller:** {}",
            permalink(repository, head, &caller.file, caller.line)
        ));
        parts.push(code_block(&caller.evidence, Info::for_path(&caller.file)));
    }
    if let Some(fix) = &finding.fix {
        parts.push(format!("**Suggested fix:** {}", prose(fix, MAX_FIX_CHARS)));
    }
    parts.push(code_block(
        &finding.evidence,
        Info::for_path(&found.file_at_head),
    ));
    if let Some(dismissed) = found.previously_dismissed {
        parts.push(dismissed_note(dismissed));
    }
    parts.push(format!(
        "React with 👍 if this finding is useful or 👎 if it is not. Fingerprint `{fp}`; an \
         eligible owner can dismiss it with `/assay dismiss {fp} <reason>`."
    ));
    parts.join("\n\n")
}

/// The note on a finding that resembles one dismissed at a lower severity (03 §3).
fn dismissed_note(dismissed: Severity) -> String {
    format!(
        "A similar finding was dismissed before, at **{}**; this one is more severe.",
        dismissed.code()
    )
}

/// The fingerprint a comment body's marker names: its first line, exactly the marker with 16
/// lowercase hex digits. Only the first line counts, because later lines may quote the
/// repository's code, which could spell a marker. Any other body names none.
pub(super) fn posted_fingerprint(body: &str) -> Option<Fingerprint> {
    let line = body.lines().next()?;
    let hex = line.strip_prefix(MARKER_START)?.strip_suffix(MARKER_END)?;
    // Text that is not a fingerprint is simply not a marker.
    hex.parse().ok()
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use assay_domain::{Caller, Confidence, FindingKind, Fingerprint, IntroducedBy, Severity};

    use super::{body, posted_fingerprint};
    use crate::config::CommitId;
    use crate::publish::findings::fixture::valid;
    use crate::publish::sanitise::Repository;
    use crate::validation::ValidFinding;

    const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

    fn head() -> CommitId {
        HEAD.parse().expect("40 lowercase hex digits")
    }

    fn repository() -> Repository<'static> {
        Repository::parse("owner/repo").expect("a valid repository")
    }

    fn rendered(found: &ValidFinding) -> String {
        body(found, repository(), &head())
    }

    #[test]
    fn a_finding_renders_with_marker_evidence_and_footer() {
        let found = valid(0xab, Severity::Major, Some(Confidence::High)).expect("fixture");

        assert_eq!(
            rendered(&found),
            "<!-- assay-finding 00000000000000ab -->\n\
             🟡 **major** · logic · defect · confidence high\n\
             \n\
             **Event recorded twice**\n\
             \n\
             The retry path records the event again\\.\n\
             \n\
             ```rust\n\
             record_event(id);\n\
             ```\n\
             \n\
             React with 👍 if this finding is useful or 👎 if it is not. Fingerprint \
             `00000000000000ab`; an eligible owner can dismiss it with \
             `/assay dismiss 00000000000000ab <reason>`."
        );
    }

    /// A finding whose text slipped past the step-5 injection screen: every construct the
    /// screen exists to catch, in every free-text field.
    fn slipped() -> ValidFinding {
        let mut found = valid(0x51, Severity::Blocker, None).expect("fixture");
        let finding = &mut found.finding;
        finding.title = "[click](https://evil.example) @owner".to_owned();
        finding.body = "![x](https://evil/p.png) <img src=x> <!-- hide -->\n\
                        ```suggestion\nrm -rf /\n```\n\
                        <https://evil> www.evil.com other/repo#1 &#64;admin"
            .to_owned();
        finding.trigger = Some("`code` [ref]: https://evil".to_owned());
        finding.expected_behavior = Some("<details>open</details>".to_owned());
        finding.rule = Some("@team/x".to_owned());
        finding.fix = Some("~~~suggestion\nevil()\n~~~".to_owned());
        finding.kind = FindingKind::Omission;
        found
    }

    #[test]
    fn a_finding_that_slipped_past_the_screen_renders_inert() {
        assert_eq!(
            rendered(&slipped()),
            "<!-- assay-finding 0000000000000051 -->\n\
             🔴 **blocker** · logic · omission · confidence unstated\n\
             \n\
             **\\[click\\]\\(https\\[\\:\\]\\/\\/evil\\.example\\) \u{ff20}owner**\n\
             \n\
             \\!\\[x\\]\\(https\\[\\:\\]\\/\\/evil\\/p\\.png\\) \\<img src\\=x\\> \
             \\<\\!\\-\\- hide \\-\\-\\>\n\
             \\`\\`\\`suggestion\n\
             rm \\-rf \\/\n\
             \\`\\`\\`\n\
             \\<https\\[\\:\\]\\/\\/evil\\> www\\[\\.\\]evil\\.com other\\/repo\u{ff03}1 \
             \\&\u{ff03}64\\;admin\n\
             \n\
             **Trigger:** \\`code\\` \\[ref\\]\\: https\\[\\:\\]\\/\\/evil\n\
             \n\
             **Expected behaviour:** \\<details\\>open\\<\\/details\\>\n\
             \n\
             **Rule:** \u{ff20}team\\/x\n\
             \n\
             **Suggested fix:** \\~\\~\\~suggestion\n\
             evil\\(\\)\n\
             \\~\\~\\~\n\
             \n\
             ```rust\n\
             record_event(id);\n\
             ```\n\
             \n\
             React with 👍 if this finding is useful or 👎 if it is not. Fingerprint \
             `0000000000000051`; an eligible owner can dismiss it with \
             `/assay dismiss 0000000000000051 <reason>`."
        );
    }

    #[test]
    fn the_only_live_link_is_the_publishers_permalink() {
        let mut found = slipped();
        found.finding.introduced_by = Some(IntroducedBy::Interaction);
        found.finding.caller = Some(Caller {
            file: "src/main.rs".to_owned(),
            line: NonZeroU64::MIN.saturating_add(9),
            evidence: "run(@x);".to_owned(),
        });

        let text = rendered(&found);

        let links: Vec<&str> = text
            .match_indices("](")
            .filter_map(|(at, _)| text.get(at..))
            .collect();
        assert_eq!(links.len(), 1, "{text}");
        assert!(
            links[0].starts_with(&format!(
                "](https://github.com/owner/repo/blob/{HEAD}/src/main.rs#L10)"
            )),
            "{text}"
        );
        assert!(
            text.contains(
                "**Caller:** [src\\/main\\.rs\\:10](https://github.com/owner/repo/blob/\
                 0123456789abcdef0123456789abcdef01234567/src/main.rs#L10)\n\
                 \n\
                 ```rust\n\
                 run(@x);\n\
                 ```\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn a_pre_existing_finding_is_marked_purple() {
        let mut found = valid(1, Severity::Minor, Some(Confidence::Low)).expect("fixture");
        found.finding.pre_existing = true;

        let text = rendered(&found);

        assert!(
            text.contains("\n🟣 **minor** · logic · defect · confidence low · pre-existing\n"),
            "{text}"
        );
    }

    #[test]
    fn a_previously_dismissed_similar_finding_says_so() {
        let mut found = valid(1, Severity::Major, None).expect("fixture");
        found.previously_dismissed = Some(Severity::Minor);

        let text = rendered(&found);

        assert!(
            text.contains(
                "\n\nA similar finding was dismissed before, at **minor**; this one is \
                 more severe.\n\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn free_text_is_capped_before_escaping() {
        let mut found = valid(1, Severity::Major, None).expect("fixture");
        found.finding.title = "t".repeat(101);
        found.finding.body = "b".repeat(1201);

        let text = rendered(&found);

        assert!(
            text.contains(&format!("**{}…**", "t".repeat(100))),
            "{text}"
        );
        assert!(
            text.contains(&format!("\n{}…\n", "b".repeat(1200))),
            "{text}"
        );
    }

    #[test]
    fn the_largest_finding_fits_a_github_comment() {
        let mut found = slipped();
        let hostile = "@<[`".repeat(1000);
        let finding = &mut found.finding;
        finding.title.clone_from(&hostile);
        finding.body.clone_from(&hostile);
        finding.trigger = Some(hostile.clone());
        finding.expected_behavior = Some(hostile.clone());
        finding.rule = Some(hostile.clone());
        finding.fix = Some(hostile.clone());
        finding.evidence = "`".repeat(10_000);
        finding.caller = Some(Caller {
            file: "p/".repeat(5000),
            line: NonZeroU64::MAX,
            evidence: "`".repeat(10_000),
        });

        let chars = rendered(&found).chars().count();

        assert!(chars < 16_000, "{chars} characters");
    }

    #[test]
    fn the_marker_names_the_posted_fingerprint() {
        let found = valid(0xab, Severity::Major, None).expect("fixture");

        let posted = posted_fingerprint(&rendered(&found));

        assert_eq!(
            posted,
            Some("00000000000000ab".parse::<Fingerprint>().expect("hex"))
        );
    }

    #[test]
    fn a_body_without_a_well_formed_marker_names_none() {
        let bodies = [
            "no marker",
            "<!-- assay-finding 00000000000000AB -->",
            "<!-- assay-finding 00000000000000ab -->x",
            "x <!-- assay-finding 00000000000000ab -->",
            "<!-- assay-finding 0ab -->",
            "quoted code:\n<!-- assay-finding 00000000000000ab -->",
        ];

        assert_eq!(bodies.map(posted_fingerprint), [None; 6]);
    }
}
