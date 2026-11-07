//! Finding fingerprints, computed here and only here (`docs/architecture/03-contracts.md` §3).

mod similarity;

use std::fmt;
use std::str::FromStr;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

use crate::finding::Class;
use crate::normalise::{has_content, normalise};

/// Hex digits in the wire form: two per byte of the 8-byte fingerprint.
const HEX_DIGITS: usize = 16;

/// What places a finding, as its fingerprint and the fuzzy dismissal match see it (03 §3).
///
/// A short-lived view over the finding and the facts Z4 established about it; nothing else about
/// the finding (title, body, fix, severity) takes part, so two differently worded reports of the
/// same defect and class share a fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor<'a> {
    /// What kind of defect it is.
    pub class: Class,
    /// The file's path at head, rename-resolved.
    pub file: &'a str,
    /// The enclosing symbol, empty at file scope.
    pub enclosing_symbol: &'a str,
    /// The finding's evidence, verbatim. The fingerprint uses its first line with content,
    /// normalised; the fuzzy match uses all of it, normalised.
    pub evidence: &'a str,
    /// For an omission, what should happen instead (empty if the finding gave none); `None`
    /// for a defect.
    pub expected_behavior: Option<&'a str>,
}

/// What an anchor expects beyond its evidence, normalised and known to have content.
enum Expectation {
    /// A defect expects nothing more.
    Defect,
    /// An omission's normalised `expected_behavior`.
    Omission(String),
}

impl Anchor<'_> {
    /// The fingerprint of this anchor's `occurrence_index`-th occurrence, counting from 0, of
    /// its first line of evidence within the file and symbol (03 §3); `None` when the anchor
    /// has no content.
    ///
    /// The digest is SHA-256 over class code, file, enclosing symbol, the first line of
    /// evidence with content (normalised; content is anything but blanks and wildcards) and,
    /// for an omission only, its normalised `expected_behavior`, each prefixed with its byte
    /// length as a big-endian `u64` so no boundary can move, then the index as a big-endian
    /// `u32`. The fingerprint is the digest's first 8 bytes.
    ///
    /// Fails closed: evidence with no line of content, or an omission whose
    /// `expected_behavior` has none, has no fingerprint, so nothing can be waived or dismissed
    /// against it. Callers dispose of such a finding as `evidence-missing`.
    #[must_use]
    pub fn fingerprint(&self, occurrence_index: u32) -> Option<Fingerprint> {
        let line = self.first_line()?;
        let expectation = self.expectation()?;
        let expected = match &expectation {
            Expectation::Defect => None,
            Expectation::Omission(text) => Some(text.as_str()),
        };
        let fields = [
            Some(self.class.code()),
            Some(self.file),
            Some(self.enclosing_symbol),
            Some(line.as_str()),
            expected,
        ];
        let mut hasher = Sha256::new();
        for field in fields.into_iter().flatten() {
            // `usize` is at most 64 bits on every target Rust supports, so this never saturates.
            let len = u64::try_from(field.len()).unwrap_or(u64::MAX);
            hasher.update(len.to_be_bytes());
            hasher.update(field);
        }
        hasher.update(occurrence_index.to_be_bytes());
        let digest: [u8; 32] = hasher.finalize().into();
        let [b0, b1, b2, b3, b4, b5, b6, b7, ..] = digest;
        Some(Fingerprint(u64::from_be_bytes([
            b0, b1, b2, b3, b4, b5, b6, b7,
        ])))
    }

    /// Whether this anchor fuzzily matches `other` (03 §3): same class, file and enclosing
    /// symbol; both evidences with content and their whole normalised texts at least 0.8
    /// similar; and both defects, or both omissions whose normalised `expected_behavior` has
    /// content and is at least 0.8 similar. Anything without content matches nothing, so it is
    /// never suppressed. Checks run cheapest first.
    pub(crate) fn resembles(&self, other: &Anchor<'_>) -> bool {
        if self.class != other.class
            || self.file != other.file
            || self.enclosing_symbol != other.enclosing_symbol
        {
            return false;
        }
        let (mine, theirs) = (normalise(self.evidence), normalise(other.evidence));
        if !has_content(&mine) || !has_content(&theirs) || !similarity::similar(&mine, &theirs) {
            return false;
        }
        match (self.expectation(), other.expectation()) {
            (Some(Expectation::Defect), Some(Expectation::Defect)) => true,
            (Some(Expectation::Omission(mine)), Some(Expectation::Omission(theirs))) => {
                similarity::similar(&mine, &theirs)
            }
            _ => false,
        }
    }

    /// The first line of evidence with content, normalised; `None` if there is none.
    fn first_line(&self) -> Option<String> {
        self.evidence
            .lines()
            .map(normalise)
            .find(|line| has_content(line))
    }

    /// What this anchor expects beyond its evidence; `None` for an omission whose
    /// `expected_behavior` has no content.
    fn expectation(&self) -> Option<Expectation> {
        match self.expected_behavior {
            None => Some(Expectation::Defect),
            Some(text) => {
                let normalised = normalise(text);
                has_content(&normalised).then_some(Expectation::Omission(normalised))
            }
        }
    }
}

/// A finding's fingerprint (03 §3), computed by [`Anchor::fingerprint`].
///
/// The first 8 bytes of a SHA-256 over its class, file, enclosing symbol, first line of evidence
/// with content, an omission's `expected_behavior`, and occurrence index.
///
/// Its wire and display form is 16 lowercase hex digits (`b49103a10654ff46`). Only Z4's value
/// is authoritative; Z3 computes the same function to apply suppressions early.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Fingerprint(u64);

/// Why text is not a fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ParseFingerprintError {
    /// The text is not 16 bytes long.
    #[error("a fingerprint is 16 hex digits, not {len} bytes")]
    Length {
        /// Its length in bytes.
        len: usize,
    },
    /// A byte is not a lowercase hex digit.
    #[error("a fingerprint is lowercase hex, but byte {index} is not")]
    Digit {
        /// The byte offset of the first such byte.
        index: usize,
    },
}

impl FromStr for Fingerprint {
    type Err = ParseFingerprintError;

    /// Parses exactly 16 lowercase hex digits, the only form assay writes.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if text.len() != HEX_DIGITS {
            return Err(ParseFingerprintError::Length { len: text.len() });
        }
        text.bytes()
            .enumerate()
            .try_fold(0_u64, |value, (index, byte)| {
                let digit = match byte {
                    b'0'..=b'9' => byte - b'0',
                    b'a'..=b'f' => byte - b'a' + 10,
                    _ => return Err(ParseFingerprintError::Digit { index }),
                };
                // Sixteen 4-bit digits fill the 64 bits exactly; no bit is shifted out.
                Ok(value << 4 | u64::from(digit))
            })
            .map(Self)
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

impl Serialize for Fingerprint {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Fingerprint {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_str(FingerprintVisitor)
    }
}

/// Reads a fingerprint from its 16-digit string.
struct FingerprintVisitor;

impl Visitor<'_> for FingerprintVisitor {
    type Value = Fingerprint;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("16 lowercase hex digits")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Fingerprint, E> {
        v.parse().map_err(E::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::{Anchor, Fingerprint, ParseFingerprintError};
    use crate::finding::Class;

    /// An anchor on `y.unwrap()` in `parse`.
    fn unwrap_in_parse(evidence: &str) -> Anchor<'_> {
        Anchor {
            class: Class::Logic,
            file: "crates/x/src/lib.rs",
            enclosing_symbol: "parse",
            evidence,
            expected_behavior: None,
        }
    }

    /// The fingerprint of the first occurrence of an anchor that has content.
    fn fp(anchor: &Anchor<'_>) -> Fingerprint {
        anchor.fingerprint(0).expect("the anchor has content")
    }

    #[test]
    fn content_less_evidence_or_expectation_has_no_fingerprint() {
        let anchors = [
            unwrap_in_parse("..."),
            unwrap_in_parse(" \n\u{2026} ...\n"),
            unwrap_in_parse(""),
            omission_in_parse("..."),
            omission_in_parse(" "),
        ];

        assert_eq!(anchors.map(|anchor| anchor.fingerprint(0)), [None; 5]);
    }

    /// An omission in `parse`, anchored on its signature, expecting `expected_behavior`.
    fn omission_in_parse(expected_behavior: &str) -> Anchor<'_> {
        Anchor {
            class: Class::Tests,
            file: "crates/x/src/lib.rs",
            enclosing_symbol: "parse",
            evidence: "pub fn parse(input: &str) -> Result<Request, ParseError>",
            expected_behavior: Some(expected_behavior),
        }
    }

    // Golden value from Python's hashlib: an omission adds its normalised `expected_behavior`
    // as a fifth length-prefixed field before the index.
    #[test]
    fn hashes_an_omission_s_expected_behavior_as_a_fifth_field() {
        let anchor = omission_in_parse("an empty input is  rejected with ParseError::Empty");

        assert_eq!(fp(&anchor).to_string(), "4fd05899e34998cf");
    }

    #[test]
    fn two_omissions_in_one_function_with_different_expectations_differ() {
        let empty = omission_in_parse("an empty input is rejected with ParseError::Empty");
        let long = omission_in_parse("an input over 1 MiB is rejected before parsing");

        assert_ne!(fp(&empty), fp(&long));
        assert!(!empty.resembles(&long));
    }

    #[test]
    fn omissions_whose_expectation_has_no_content_resemble_nothing() {
        for (left, right) in [("...", "\u{2026}"), (" \n\t", ""), ("...", "...")] {
            assert!(
                !omission_in_parse(left).resembles(&omission_in_parse(right)),
                "{left:?} vs {right:?}"
            );
        }
    }

    #[test]
    fn skips_blank_and_elision_only_lines_to_the_first_line_with_content() {
        let elided = unwrap_in_parse("...\n  \u{2026}  ... \n\n let x = y.unwrap();");

        assert_eq!(fp(&elided), fp(&unwrap_in_parse("let x = y.unwrap();")));
    }

    #[test]
    fn different_lines_behind_the_same_elision_differ() {
        let first = unwrap_in_parse("...\nlet x = y.unwrap();");
        let second = unwrap_in_parse("...\ndrop(guard);");

        assert_ne!(fp(&first), fp(&second));
    }

    #[test]
    fn a_comment_only_first_line_is_content() {
        let commented = unwrap_in_parse("// SAFETY: checked above\nlet x = y.unwrap();");

        assert_eq!(
            fp(&commented),
            fp(&unwrap_in_parse("// SAFETY: checked above"))
        );
    }

    #[test]
    fn evidence_without_content_resembles_nothing() {
        for (left, right) in [("...", "..."), ("", ""), (" \n\u{2026}", "...")] {
            assert!(
                !unwrap_in_parse(left).resembles(&unwrap_in_parse(right)),
                "{left:?} vs {right:?}"
            );
        }
    }

    #[test]
    fn resembles_over_the_whole_evidence_not_only_the_first_line() {
        let dismissed = unwrap_in_parse("let x = y.unwrap();\nfoo_bar_baz_qux_quux();");

        assert!(!unwrap_in_parse("let x = y.unwrap();\na();").resembles(&dismissed));
    }

    // Golden values from Python's hashlib over the same length-prefixed fields: any change to
    // the encoding orphans every recorded dismissal and waiver.
    #[test]
    fn hashes_length_prefixed_fields_with_sha256() {
        let anchor = unwrap_in_parse("let x = y.unwrap();");

        let fingerprints = (anchor.fingerprint(0), anchor.fingerprint(1));

        assert_eq!(
            (
                fingerprints.0.map(|fp| fp.to_string()),
                fingerprints.1.map(|fp| fp.to_string())
            ),
            (
                Some("b49103a10654ff46".to_owned()),
                Some("2fb44b0e6d1fa041".to_owned())
            )
        );
    }

    #[test]
    fn hashes_only_the_first_non_blank_line_of_evidence_normalised() {
        let multi_line = unwrap_in_parse("\n  \n let  x = y.unwrap(); // why\n    .map(f);");

        assert_eq!(fp(&multi_line), fp(&unwrap_in_parse("let x = y.unwrap();")));
    }

    #[test]
    fn every_field_changes_the_fingerprint() {
        let base = unwrap_in_parse("let x = y.unwrap();");
        let variants = [
            Anchor {
                class: Class::ErrorHandling,
                ..base
            },
            Anchor {
                file: "crates/x/src/main.rs",
                ..base
            },
            Anchor {
                enclosing_symbol: "parse_all",
                ..base
            },
            unwrap_in_parse("let x = z.unwrap();"),
        ];

        for variant in variants {
            assert_ne!(fp(&variant), fp(&base), "{variant:?}");
        }
    }

    #[test]
    fn resembles_a_reworded_quote_of_the_same_line() {
        let dismissed = unwrap_in_parse("let x = y.unwrap(); // ok");

        let reworded = unwrap_in_parse("let  x = y.unwrap()");

        assert!(reworded.resembles(&dismissed));
    }

    #[test]
    fn does_not_resemble_another_file_symbol_or_class() {
        let base = unwrap_in_parse("let x = y.unwrap();");
        let others = [
            Anchor {
                file: "crates/x/src/main.rs",
                ..base
            },
            Anchor {
                enclosing_symbol: "parse_all",
                ..base
            },
            Anchor {
                class: Class::Resource,
                ..base
            },
        ];

        for other in others {
            assert!(!other.resembles(&base), "{other:?}");
        }
    }

    #[test]
    fn does_not_resemble_a_different_line() {
        let base = unwrap_in_parse("let x = y.unwrap();");

        assert!(!unwrap_in_parse("let total = sum(values)?;").resembles(&base));
    }

    #[test]
    fn a_field_boundary_cannot_be_moved() {
        let split_one_way = Anchor {
            file: "a/b",
            enclosing_symbol: "c",
            ..unwrap_in_parse("x")
        };
        let split_another_way = Anchor {
            file: "a/",
            enclosing_symbol: "bc",
            ..unwrap_in_parse("x")
        };

        assert_ne!(fp(&split_one_way), fp(&split_another_way));
    }

    #[test]
    fn displays_as_sixteen_lowercase_hex_digits_and_parses_back() {
        let text = "00ff10a0b49103a1";

        let parsed: Result<Fingerprint, _> = text.parse();

        assert_eq!(parsed.map(|fp| fp.to_string()), Ok(text.to_owned()));
    }

    #[test]
    fn refuses_a_wrong_length() {
        assert_eq!(
            "0123".parse::<Fingerprint>(),
            Err(ParseFingerprintError::Length { len: 4 })
        );
    }

    #[test]
    fn refuses_uppercase_and_signs() {
        assert_eq!(
            "0123456789ABCDEF".parse::<Fingerprint>(),
            Err(ParseFingerprintError::Digit { index: 10 })
        );
        assert_eq!(
            "+123456789abcdef".parse::<Fingerprint>(),
            Err(ParseFingerprintError::Digit { index: 0 })
        );
    }
}
