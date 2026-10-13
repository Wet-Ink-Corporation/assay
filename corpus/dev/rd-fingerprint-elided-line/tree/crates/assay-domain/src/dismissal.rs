//! An owner's dismissal and how a later finding matches it (`docs/architecture/03-contracts.md`
//! §3, 04 §6).

use crate::fingerprint::Anchor;
use crate::severity::Severity;

/// A finding an eligible owner dismissed, as the fuzzy match needs it: where it was and the
/// severity it had, which is the ceiling of what the dismissal suppresses (04 §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dismissal<'a> {
    /// Where the dismissed finding was.
    pub anchor: Anchor<'a>,
    /// Its severity when dismissed.
    pub severity: Severity,
}

/// How a finding stands against a dismissal (03 §3, R3-25).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DismissalMatch {
    /// The finding does not resemble the dismissed one; publish it as usual.
    Unrelated,
    /// The dismissed finding again, at or below its severity; suppress it.
    Suppressed,
    /// It resembles the dismissed finding but is more severe; publish it with the note
    /// "previously dismissed similar (`dismissed`)".
    MoreSevere {
        /// The severity of the dismissed finding.
        dismissed: Severity,
    },
}

impl Dismissal<'_> {
    /// Judges a finding at `anchor` with `severity` against this dismissal: it matches when it
    /// is in the same file and enclosing symbol, of the same class, and its normalised first
    /// line of evidence is at least 0.8 similar to the dismissed one's.
    #[must_use]
    pub fn judge(&self, anchor: &Anchor<'_>, severity: Severity) -> DismissalMatch {
        if !anchor.resembles(&self.anchor) {
            DismissalMatch::Unrelated
        } else if severity <= self.severity {
            DismissalMatch::Suppressed
        } else {
            DismissalMatch::MoreSevere {
                dismissed: self.severity,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Dismissal, DismissalMatch};
    use crate::finding::Class;
    use crate::fingerprint::Anchor;
    use crate::severity::Severity;

    /// An anchor in `parse` on `evidence`.
    fn in_parse(evidence: &str) -> Anchor<'_> {
        Anchor {
            class: Class::ErrorHandling,
            file: "src/lib.rs",
            enclosing_symbol: "parse",
            evidence,
        }
    }

    /// A dismissal of a `major` on `y.unwrap()` in `parse`.
    fn dismissed_major() -> Dismissal<'static> {
        Dismissal {
            anchor: in_parse("let x = y.unwrap();"),
            severity: Severity::Major,
        }
    }

    #[test]
    fn suppresses_a_resembling_finding_at_or_below_the_dismissed_severity() {
        let dismissal = dismissed_major();
        let candidate = in_parse("let x = y.unwrap()");

        let outcomes = [Severity::Major, Severity::Minor, Severity::Nit]
            .map(|severity| dismissal.judge(&candidate, severity));

        assert_eq!(outcomes, [DismissalMatch::Suppressed; 3]);
    }

    #[test]
    fn flags_a_resembling_finding_above_the_dismissed_severity() {
        let outcome = dismissed_major().judge(&in_parse("let x = y.unwrap()"), Severity::Blocker);

        assert_eq!(
            outcome,
            DismissalMatch::MoreSevere {
                dismissed: Severity::Major
            }
        );
    }

    #[test]
    fn leaves_a_finding_that_does_not_resemble_it_alone() {
        let outcome = dismissed_major().judge(&in_parse("drop(guard);"), Severity::Nit);

        assert_eq!(outcome, DismissalMatch::Unrelated);
    }
}
