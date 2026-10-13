//! The free-text egress scan (03 §3, 05 T2, T8 and §5): a steered model needs no network to
//! leak the OAuth token, only a finding someone reads. Every field a reviewer wrote is scanned
//! for the token and its common encodings, and for long runs that nothing in the tree explains;
//! any hit drops the finding as `injection-suspect`, fails the `review` job and flags the token
//! for rotation. Uploaded artifacts are scanned for token-shaped strings.
//!
//! The token is encoded and the text normalised, never the text decoded: that is linear and
//! needs no parser of the encodings. Where an encoding is not covered, the unexplained-run rule
//! is the catch-all, so what is not provably the tree's own text is a hit (wi-b5affa).

mod encoding;
mod views;

use std::collections::HashSet;
use std::fmt;

use assay_domain::Finding;
use thiserror::Error;

use super::FindingRef;
use super::screen;
use crate::config::CommitId;

/// How many normalised characters of the token a window holds (03 §3: at least 12). A longer
/// match holds a window of this length, so only this length is looked for.
const WINDOW: usize = 12;

/// The fewest characters of a run that must occur in the tree or base `REVIEW.md` (03 §3).
const MIN_UNEXPLAINED_RUN: usize = 20;

/// The most bytes [`EgressToken::new`] accepts: the token is the owner's secret, but its
/// windows are built once per byte, so their number is bounded too (H-13).
pub const MAX_TOKEN_BYTES: usize = 1024;

/// The most bytes [`KnownRuns::index`] reads, summed over its texts; the decide job's budget for
/// a tree's evidence reads is the same (`MAX_EVIDENCE_BYTES`).
pub const MAX_INDEXED_BYTES: usize = 64 * 1024 * 1024;

/// What the egress scan found. Each names a category, never the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EgressKind {
    /// Twelve or more normalised characters of the token, of its base64 (at any of the three
    /// alignments, in either alphabet) or of its hex (in either case).
    Token,
    /// A run of 20 or more alphanumeric (or invisible) characters found in neither the tree nor
    /// base `REVIEW.md`.
    UnexplainedRun,
    /// A character drawn like an ASCII letter or digit from a block normalisation cannot read
    /// back: mathematical alphanumerics, enclosed alphanumerics, superscripts and subscripts,
    /// small capitals and modifier letters, and any non-ASCII numeric character.
    Obfuscated,
    /// In an artifact: a token-shaped string the tree does not explain (a credential prefix, a
    /// private key header, an AWS access key id, or a long run mixing letters and digits that
    /// is not a hex digest).
    TokenShaped,
}

/// A field of a finding that the reviewer wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FreeTextField {
    /// `title`.
    Title,
    /// `body`.
    Body,
    /// `fix`.
    Fix,
    /// `trigger`.
    Trigger,
    /// `evidence`, as the reviewer quoted it.
    Evidence,
    /// `expected_behavior`.
    ExpectedBehavior,
    /// `group_hint`.
    GroupHint,
    /// `rule`.
    Rule,
    /// An interaction's `caller.evidence`, as the reviewer quoted it.
    CallerEvidence,
    /// A whole document that failed its schema, scanned raw and decoded for the token (Z3).
    RejectedDocument,
}

/// One finding the egress scan dropped: the first field it hit in, and what it found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EgressHit {
    /// The finding.
    pub at: FindingRef,
    /// The first field, in [`FreeTextField`] order, that holds a hit.
    pub field: FreeTextField,
    /// What that field holds.
    pub kind: EgressKind,
}

/// The token could not be scanned for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum EgressTokenError {
    /// Normalised, the token has fewer characters than one window.
    #[error("the token has fewer than {min} alphanumeric characters")]
    TooShort {
        /// The fewest it needs.
        min: usize,
    },
    /// The token is longer than [`MAX_TOKEN_BYTES`].
    #[error("the token is longer than {max} bytes")]
    TooLong {
        /// The most it may hold.
        max: usize,
    },
}

/// The tree's texts could not be indexed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum KnownRunsError {
    /// The texts hold more than [`MAX_INDEXED_BYTES`] together; none past the cap was read.
    #[error("the tree holds more than {limit} bytes to index")]
    TooLarge {
        /// The cap.
        limit: usize,
    },
}

/// The OAuth token, held as the windows the scan looks for. Z3 holds it; Z4 does not (05 §4).
pub struct EgressToken {
    /// Every window of the token's normalised forms: plain, base64 at three alignments, hex.
    windows: HashSet<[u8; WINDOW]>,
}

impl fmt::Debug for EgressToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EgressToken").finish_non_exhaustive()
    }
}

impl EgressToken {
    /// The windows of `token` and of its base64 and hex encodings.
    ///
    /// Base64 is taken over the token starting at each of its first three bytes, so it is found
    /// wherever it falls in a three-byte group of a longer encoded text.
    ///
    /// # Errors
    ///
    /// [`EgressTokenError::TooLong`] past [`MAX_TOKEN_BYTES`], and
    /// [`EgressTokenError::TooShort`] when fewer than 12 alphanumeric characters remain once
    /// normalised: such a token could not be told apart from ordinary text.
    pub fn new(token: &str) -> Result<Self, EgressTokenError> {
        if token.len() > MAX_TOKEN_BYTES {
            return Err(EgressTokenError::TooLong {
                max: MAX_TOKEN_BYTES,
            });
        }
        let plain = normalise(token);
        if plain.len() < WINDOW {
            return Err(EgressTokenError::TooShort { min: WINDOW });
        }
        let bytes = token.as_bytes();
        let mut forms = vec![plain, encoding::hex(bytes).into_bytes()];
        forms.extend(
            (0..3)
                .filter_map(|skip| bytes.get(skip..))
                .map(|from| normalise(&encoding::base64_whole_groups(from))),
        );
        let windows = forms
            .iter()
            .flat_map(|form| form.array_windows::<WINDOW>().copied())
            .collect();
        Ok(Self { windows })
    }

    /// Whether `text` holds one of the token's windows, normalised or read as per-byte hex.
    fn is_in(&self, text: &str) -> bool {
        [normalise(text), views::hex_view(text)].iter().any(|view| {
            view.array_windows::<WINDOW>()
                .any(|window| self.windows.contains(window))
        })
    }
}

/// The runs of 20 or more characters in the PR tree and base `REVIEW.md`.
///
/// They are what a finding may quote at length. A run is found by whole-run equality, so a part
/// of a longer identifier is not explained by it (the stricter reading, and linear).
#[derive(Default)]
pub struct KnownRuns<'t> {
    runs: HashSet<&'t str>,
}

impl fmt::Debug for KnownRuns<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KnownRuns")
            .field("runs", &self.runs.len())
            .finish()
    }
}

impl<'t> KnownRuns<'t> {
    /// Indexes the long runs of `texts`: every file of the PR tree the caller read, and base
    /// `REVIEW.md`. Bytes that are not UTF-8 end a run. Leaving a text out only makes more runs
    /// unexplained, so the caller may skip binary files. [`KnownRuns::default`] explains nothing.
    ///
    /// Cost: linear in the bytes read, which stop at [`MAX_INDEXED_BYTES`].
    ///
    /// # Errors
    ///
    /// [`KnownRunsError::TooLarge`] when the texts hold more than [`MAX_INDEXED_BYTES`]
    /// together, before the text that crosses the cap is read. The caller then has no index and
    /// fails closed: it fails the job, or scans with [`KnownRuns::default`].
    pub fn index(texts: impl IntoIterator<Item = &'t [u8]>) -> Result<Self, KnownRunsError> {
        let mut runs = HashSet::new();
        let mut indexed: usize = 0;
        for text in texts {
            indexed = indexed
                .checked_add(text.len())
                .filter(|&total| total <= MAX_INDEXED_BYTES)
                .ok_or(KnownRunsError::TooLarge {
                    limit: MAX_INDEXED_BYTES,
                })?;
            for chunk in text.utf8_chunks() {
                runs.extend(long_runs(chunk.valid()));
            }
        }
        Ok(Self { runs })
    }

    /// Adds the run's own base and head commits, which assay knows and the model did not
    /// choose, so a permalink to them is explained. Equality is exact: a commit id is one run.
    #[must_use]
    pub fn with_commits(mut self, commits: impl IntoIterator<Item = &'t CommitId>) -> Self {
        self.runs.extend(commits.into_iter().map(CommitId::as_str));
        self
    }

    fn explains(&self, run: &str) -> bool {
        self.runs.contains(run)
    }
}

/// What the egress scan compares model text with: the token when the zone holds it, and the
/// runs the tree explains.
#[derive(Debug)]
pub struct EgressScan<'t> {
    token: Option<&'t EgressToken>,
    known: KnownRuns<'t>,
}

impl<'t> EgressScan<'t> {
    /// The scan Z3 runs: for the token and its encodings, and for unexplained runs.
    #[must_use]
    pub const fn with_token(token: &'t EgressToken, known: KnownRuns<'t>) -> Self {
        Self {
            token: Some(token),
            known,
        }
    }

    /// The scan Z4 runs: it holds no token (05 §4), so only for unexplained runs, which an
    /// encoding of the token of 20 or more characters is.
    #[must_use]
    pub const fn without_token(known: KnownRuns<'t>) -> Self {
        Self { token: None, known }
    }

    /// Scans an artifact before the review job uploads it (05 §5): its bytes as text and, if it
    /// is strict JSON, every key and string decoded; if not, its text with the escapes read back,
    /// so escapes cannot split a token either way. An escape that cannot be read back is
    /// `TokenShaped`, and a lookalike alphanumeric `Obfuscated`, as in a finding (wi-b5affa). The run rule is not applied: artifacts carry digests and keys
    /// that assay stamps itself.
    ///
    /// Cost: linear in `artifact`, plus one parse.
    #[must_use]
    pub fn scan_artifact(&self, artifact: &[u8]) -> Option<EgressKind> {
        views::scan_views(artifact, |text| self.artifact_text(text))
            .unwrap_or(Some(EgressKind::TokenShaped))
    }

    /// Scans a document that failed its schema for the token alone (Z3 only: without the token
    /// there is nothing to look for, and the document's stamped fields would trip the other
    /// rules), raw and decoded as [`EgressScan::scan_artifact`] reads it.
    pub(super) fn scan_rejected(&self, document: &[u8]) -> Option<EgressKind> {
        let token = self.token?;
        views::scan_views(document, |text| {
            token.is_in(text).then_some(EgressKind::Token)
        })
        .unwrap_or(Some(EgressKind::TokenShaped))
    }

    /// The first field of `finding` the reviewer wrote that holds a hit, and what it holds.
    pub(super) fn scan_finding(&self, finding: &Finding) -> Option<(FreeTextField, EgressKind)> {
        model_text(finding).find_map(|(field, text)| Some((field, self.scan_text(text)?)))
    }

    fn scan_text(&self, text: &str) -> Option<EgressKind> {
        if self.holds_token(text) {
            Some(EgressKind::Token)
        } else if text.chars().any(views::is_obfuscated) {
            Some(EgressKind::Obfuscated)
        } else if long_runs(text).any(|run| !self.known.explains(run)) {
            Some(EgressKind::UnexplainedRun)
        } else {
            None
        }
    }

    fn artifact_text(&self, text: &str) -> Option<EgressKind> {
        if self.holds_token(text) {
            Some(EgressKind::Token)
        } else if text.chars().any(views::is_obfuscated) {
            Some(EgressKind::Obfuscated)
        } else if screen::token_shaped(text, |run| self.known.explains(run)) {
            Some(EgressKind::TokenShaped)
        } else {
            None
        }
    }

    fn holds_token(&self, text: &str) -> bool {
        self.token.is_some_and(|token| token.is_in(text))
    }
}

/// `text` as the token windows are compared with it: folded as a reader sees it (invisible
/// characters dropped, lowercase, lookalike letters read as Latin), then only ASCII letters
/// and digits kept. Separators, case, and the difference between the two base64 alphabets are
/// gone.
fn normalise(text: &str) -> Vec<u8> {
    screen::fold(text)
        .bytes()
        .filter(u8::is_ascii_alphanumeric)
        .collect()
}

/// The runs of `text` of at least [`MIN_UNEXPLAINED_RUN`] characters. A run is a maximal
/// sequence of alphanumeric characters (any script) and invisible ones, so a zero-width
/// character cannot cut a long run into short ones.
fn long_runs(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_alphanumeric() || screen::is_invisible(c)))
        // A run has at least as many bytes as characters: the byte test skips counting short ones.
        .filter(|run| run.len() >= MIN_UNEXPLAINED_RUN)
        .filter(|run| run.chars().count() >= MIN_UNEXPLAINED_RUN)
}

/// Every field of `finding` the reviewer wrote, with its name. `evidence` and `caller.evidence`
/// are read as written: the scan runs before step 2 replaces them with the tree's lines.
fn model_text(finding: &Finding) -> impl Iterator<Item = (FreeTextField, &str)> {
    [
        (FreeTextField::Title, Some(finding.title.as_str())),
        (FreeTextField::Body, Some(finding.body.as_str())),
        (FreeTextField::Fix, finding.fix.as_deref()),
        (FreeTextField::Trigger, finding.trigger.as_deref()),
        (FreeTextField::Evidence, Some(finding.evidence.as_str())),
        (
            FreeTextField::ExpectedBehavior,
            finding.expected_behavior.as_deref(),
        ),
        (FreeTextField::GroupHint, finding.group_hint.as_deref()),
        (FreeTextField::Rule, finding.rule.as_deref()),
        (
            FreeTextField::CallerEvidence,
            finding
                .caller
                .as_ref()
                .map(|caller| caller.evidence.as_str()),
        ),
    ]
    .into_iter()
    .filter_map(|(field, text)| Some((field, text?)))
}

#[cfg(test)]
mod tests {
    use super::{
        EgressToken, EgressTokenError, KnownRuns, KnownRunsError, MAX_INDEXED_BYTES,
        MAX_TOKEN_BYTES,
    };

    #[test]
    fn refuses_a_token_of_eleven_normalised_characters_and_takes_twelve() {
        let eleven = EgressToken::new("ab-cd-ef-gh-ijk").map(|_| ());
        let twelve = EgressToken::new("ab-cd-ef-gh-ijkl").map(|_| ());

        assert_eq!(
            (eleven, twelve),
            (Err(EgressTokenError::TooShort { min: 12 }), Ok(()))
        );
    }

    #[test]
    fn refuses_a_token_past_its_byte_cap() {
        let at_cap = "a".repeat(MAX_TOKEN_BYTES);
        let past_cap = "a".repeat(MAX_TOKEN_BYTES + 1);

        assert_eq!(
            (
                EgressToken::new(&at_cap).map(|_| ()),
                EgressToken::new(&past_cap).map(|_| ())
            ),
            (
                Ok(()),
                Err(EgressTokenError::TooLong {
                    max: MAX_TOKEN_BYTES
                })
            )
        );
    }

    #[test]
    fn never_shows_the_token_in_debug_output() {
        let token = EgressToken::new("sk-ant-oat01-secretsecret").expect("long enough");

        assert_eq!(format!("{token:?}"), "EgressToken { .. }");
    }

    #[test]
    fn indexes_up_to_its_byte_cap_and_refuses_past_it() {
        // One shared mebibyte of spaces, passed as many texts: no run, no extra memory.
        let mebibyte = vec![b' '; 1024 * 1024];
        let at_cap = vec![mebibyte.as_slice(); MAX_INDEXED_BYTES / mebibyte.len()];
        let past_cap = at_cap.iter().copied().chain([b" ".as_slice()]);

        assert_eq!(
            (
                KnownRuns::index(at_cap.iter().copied()).map(|known| known.runs.len()),
                KnownRuns::index(past_cap).map(|known| known.runs.len())
            ),
            (
                Ok(0),
                Err(KnownRunsError::TooLarge {
                    limit: MAX_INDEXED_BYTES
                })
            )
        );
    }

    #[test]
    fn ends_runs_at_bytes_that_are_not_utf8() {
        let text = b"abcdefghij\xffklmnopqrst abcdefghijklmnopqrstu".as_slice();

        let known = KnownRuns::index([text]).expect("far below the cap");

        assert_eq!(
            known.runs.into_iter().collect::<Vec<_>>(),
            ["abcdefghijklmnopqrstu"]
        );
    }
}
