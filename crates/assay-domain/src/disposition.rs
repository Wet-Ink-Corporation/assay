//! Dispositions: what happened to an individual finding (`docs/architecture/03-contracts.md` §5).

/// What happened to one finding on its way to the decision. A closed set (03 §5); not every
/// disposition drops the finding (`NoTrigger` caps it at `minor`, `LineNotInDiffMoved` moves it to
/// the summary).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Disposition {
    /// The document failed its schema: unknown field or version, over-length text, wrong shape.
    Schema,
    /// The quoted evidence occurs nowhere in the file.
    EvidenceMissing,
    /// The line is outside every diff hunk, so the finding moves to the summary.
    LineNotInDiffMoved,
    /// Another finding already reports the same defect.
    Duplicate,
    /// A gate already reports the same defect.
    DuplicateOfGate,
    /// A base `suppress` rule matches the finding.
    SuppressedByRule,
    /// The owner dismissed the finding.
    DismissedByOwner,
    /// The owner waived the finding.
    WaivedByOwner,
    /// The verifier refuted the finding, which is downgraded to `minor`.
    VerifierRejectedDowngraded,
    /// A per-run or per-role cap on findings was reached.
    CapExceeded,
    /// The finding is about code the diff did not introduce.
    PreExisting,
    /// The free-text egress scan flagged the finding.
    InjectionSuspect,
    /// A `major` or `blocker` without a trigger, capped at `minor`.
    NoTrigger,
}

impl Disposition {
    /// Every disposition, in the order 03 §5 lists them.
    pub const ALL: [Self; 13] = [
        Self::Schema,
        Self::EvidenceMissing,
        Self::LineNotInDiffMoved,
        Self::Duplicate,
        Self::DuplicateOfGate,
        Self::SuppressedByRule,
        Self::DismissedByOwner,
        Self::WaivedByOwner,
        Self::VerifierRejectedDowngraded,
        Self::CapExceeded,
        Self::PreExisting,
        Self::InjectionSuspect,
        Self::NoTrigger,
    ];

    /// The wire code of this disposition in `assay.decision/1` (`dispositions[].code`).
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Schema => "schema",
            Self::EvidenceMissing => "evidence-missing",
            Self::LineNotInDiffMoved => "line-not-in-diff-moved",
            Self::Duplicate => "duplicate",
            Self::DuplicateOfGate => "duplicate-of-gate",
            Self::SuppressedByRule => "suppressed-by-rule",
            Self::DismissedByOwner => "dismissed-by-owner",
            Self::WaivedByOwner => "waived-by-owner",
            Self::VerifierRejectedDowngraded => "verifier-rejected-downgraded",
            Self::CapExceeded => "cap-exceeded",
            Self::PreExisting => "pre-existing",
            Self::InjectionSuspect => "injection-suspect",
            Self::NoTrigger => "no-trigger",
        }
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
