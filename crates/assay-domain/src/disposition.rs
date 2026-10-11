//! Dispositions: what happened to an individual finding (`docs/architecture/03-contracts.md` §5).

use crate::vocabulary::closed_enum;

closed_enum! {
    /// What happened to one finding on its way to the decision. A closed set (03 §5).
    ///
    /// Not every disposition drops the finding (`NoTrigger` caps it at `minor`,
    /// `LineNotInDiffMoved` moves it to the summary). Its `code()` is `assay.decision/1`
    /// `dispositions[].code`.
    #[derive(PartialOrd, Ord)]
    pub enum Disposition {
        /// The document failed its schema: unknown field or version, over-length text, wrong shape.
        Schema => "schema",
        /// The quoted evidence occurs nowhere in the file.
        EvidenceMissing => "evidence-missing",
        /// The line is outside every diff hunk, so the finding moves to the summary.
        LineNotInDiffMoved => "line-not-in-diff-moved",
        /// Another finding already reports the same defect.
        Duplicate => "duplicate",
        /// A gate already reports the same defect.
        DuplicateOfGate => "duplicate-of-gate",
        /// A base `suppress` rule matches the finding.
        SuppressedByRule => "suppressed-by-rule",
        /// The owner dismissed the finding.
        DismissedByOwner => "dismissed-by-owner",
        /// The owner waived the finding.
        WaivedByOwner => "waived-by-owner",
        /// The verifier refuted the finding, which is downgraded to `minor`.
        VerifierRejectedDowngraded => "verifier-rejected-downgraded",
        /// A per-run or per-role cap on findings was reached.
        CapExceeded => "cap-exceeded",
        /// The finding is about code the diff did not introduce.
        PreExisting => "pre-existing",
        /// The free-text egress scan flagged the finding.
        InjectionSuspect => "injection-suspect",
        /// A `major` or `blocker` without a trigger, capped at `minor`.
        NoTrigger => "no-trigger",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn schema_disposition_has_wire_code_schema() {
        let code = Disposition::Schema.code();

        assert_eq!(code, "schema");
    }

    #[test]
    fn every_disposition_has_a_distinct_kebab_case_code() {
        let codes: BTreeSet<&str> = Disposition::ALL.iter().map(|d| d.code()).collect();

        assert_eq!(
            codes.len(),
            Disposition::ALL.len(),
            "codes collide: {codes:?}"
        );
        assert!(
            codes
                .iter()
                .all(|code| code.chars().all(|c| c.is_ascii_lowercase() || c == '-')),
            "codes are kebab-case: {codes:?}"
        );
    }
}
