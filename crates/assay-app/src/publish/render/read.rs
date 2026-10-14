//! Reads back the verdict check's `output.text` exactly as [`super::text`] writes it, and a
//! refusal's `output.title` exactly as [`super::title`] writes it, for `assay status`. Any other
//! text is refused (wi-b5affa): the reader never guesses at a format.

use std::num::NonZeroU64;

use assay_domain::ReasonCode;

use crate::config::Sha256Digest;
use crate::publish::ports::EvidencePointer;

/// What a verdict check's `output.text` projects of the ledger state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectedText<'a> {
    /// The decision key the check was written under.
    pub(crate) decision_key: &'a str,
    /// The decision's inputs digest: `None` exactly when the run was refused before the rubric
    /// (a decided run always writes it).
    pub(crate) inputs_digest: Option<&'a str>,
    /// The evidence pointer, `None` when the run persisted no bundle.
    pub(crate) pointer: Option<EvidencePointer>,
}

/// The reason code of a refusal's `output.title`, read as [`super::title`] writes it for a
/// `failure` with one reason (`assay: failure (<code>)`), or `None` for any other title: more
/// than one reason, another conclusion, or a code outside 03's closed list.
pub(crate) fn read_refusal_title(title: &str) -> Option<ReasonCode> {
    let code = title.strip_prefix("assay: failure (")?.strip_suffix(')')?;
    ReasonCode::ALL
        .into_iter()
        .find(|reason| reason.code() == code)
}

/// `text` read as [`super::text`] writes it, or `None` when it is anything else.
pub(crate) fn read_text(text: &str) -> Option<ProjectedText<'_>> {
    let mut lines = text.split('\n');
    if lines.next()? != "```text" {
        return None;
    }
    let decision_key = lines.next()?.strip_prefix("decision_key: ")?;
    let mut line = lines.next()?;
    let mut inputs_digest = None;
    if let Some(digest) = line.strip_prefix("inputs_digest: ") {
        digest.parse::<Sha256Digest>().ok()?;
        inputs_digest = Some(digest);
        line = lines.next()?;
    }
    let pointer = match line.strip_prefix("evidence pointer: ")? {
        "none" => None,
        pointer => {
            let pointer = read_pointer(pointer)?;
            lines.next()?.strip_prefix("evidence record: ")?;
            Some(pointer)
        }
    };
    let closed = lines.next()? == "```" && lines.next().is_none();
    closed.then_some(ProjectedText {
        decision_key,
        inputs_digest,
        pointer,
    })
}

/// `run_id <n>, run_attempt <n>, sha256 <digest>`.
fn read_pointer(text: &str) -> Option<EvidencePointer> {
    let rest = text.strip_prefix("run_id ")?;
    let (run_id, rest) = rest.split_once(", run_attempt ")?;
    let (run_attempt, sha256) = rest.split_once(", sha256 ")?;
    Some(EvidencePointer {
        run_id: read_count(run_id)?,
        run_attempt: read_count(run_attempt)?,
        sha256: sha256.parse().ok()?,
    })
}

/// A positive count written in ASCII digits only (`u64`'s parser also takes a leading `+`).
fn read_count(text: &str) -> Option<NonZeroU64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use assay_domain::{Conclusion, Reason, ReasonCode, Tier};

    use super::{ProjectedText, read_refusal_title, read_text};
    use crate::publish::ports::{EvidenceEntry, EvidencePointer, EvidenceRecord, LlmEvidence};
    use crate::publish::render::{Projection, text, title};

    const KEY: &str = "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
    const DIGEST: &str = "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

    fn pointer() -> EvidencePointer {
        EvidencePointer {
            run_id: NonZeroU64::new(7).expect("not zero"),
            run_attempt: NonZeroU64::MIN,
            sha256: DIGEST.parse().expect("a sha256 digest"),
        }
    }

    fn entry() -> EvidenceEntry {
        EvidenceEntry {
            repo_id: NonZeroU64::new(840_213_377).expect("not zero"),
            pr: NonZeroU64::new(42).expect("not zero"),
            pointer: pointer(),
            record: EvidenceRecord {
                evidence_key: "sha256:run".to_owned(),
                bundle_sha256: DIGEST.parse().expect("a sha256 digest"),
                llm: LlmEvidence::Complete,
                tier: Tier::Micro,
            },
        }
    }

    #[test]
    fn reads_back_a_written_pointer() {
        let entry = entry();
        let written = text(&Projection {
            decision_key: KEY,
            inputs_digest: Some(DIGEST),
            evidence: Some(&entry),
        });

        assert_eq!(
            read_text(&written),
            Some(ProjectedText {
                decision_key: KEY,
                inputs_digest: Some(DIGEST),
                pointer: Some(pointer()),
            })
        );
    }

    #[test]
    fn reads_back_a_run_that_persisted_nothing() {
        let written = text(&Projection {
            decision_key: KEY,
            inputs_digest: None,
            evidence: None,
        });

        assert_eq!(
            read_text(&written),
            Some(ProjectedText {
                decision_key: KEY,
                inputs_digest: None,
                pointer: None,
            })
        );
    }

    #[test]
    fn reads_back_every_refusal_title() {
        for code in ReasonCode::ALL {
            let reason = Reason {
                code,
                detail: "detail".to_owned(),
                fp: None,
            };
            let written = title(Conclusion::Failure, &[reason]);

            assert_eq!(read_refusal_title(&written), Some(code), "{written}");
        }
    }

    #[test]
    fn refuses_titles_that_are_not_one_failure_reason() {
        let reason = |code| Reason {
            code,
            detail: "detail".to_owned(),
            fp: None,
        };
        let two = [
            reason(ReasonCode::GateError),
            reason(ReasonCode::PlanMismatch),
        ];
        let cases = [
            title(Conclusion::Failure, &[]),
            title(Conclusion::Failure, &two),
            title(Conclusion::Success, &[reason(ReasonCode::GateError)]),
            title(Conclusion::Neutral, &[reason(ReasonCode::Override)]),
            "assay: failure (unknown-code)".to_owned(),
            "assay: failure (GATE-ERROR)".to_owned(),
            "assay: failure (gate-error))".to_owned(),
        ];

        for case in cases {
            assert_eq!(read_refusal_title(&case), None, "{case:?} must be refused");
        }
    }

    #[test]
    fn refuses_text_it_did_not_write() {
        let cases = [
            "",
            "decision_key: k",
            "```text\ndecision_key: k\nevidence pointer: none\n```\nmore",
            "```text\ndecision_key: k\nevidence pointer: none\n```\n",
            "```text\ndecision_key: k\ninputs_digest: x\nevidence pointer: none\n```",
            "```text\ndecision_key: k\nevidence pointer: maybe\n```",
            "```text\ndecision_key: k\nevidence pointer: run_id 7, run_attempt 1, sha256 \
             sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee\n```",
            "```text\ndecision_key: k\nevidence pointer: run_id +7, run_attempt 1, sha256 \
             sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee\n\
             evidence record: x\n```",
            "```text\ndecision_key: k\nevidence pointer: run_id 0, run_attempt 1, sha256 \
             sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee\n\
             evidence record: x\n```",
            "```text\ndecision_key: k\nevidence pointer: run_id 7, run_attempt 1, sha256 \
             sha256:EEEE\nevidence record: x\n```",
        ];

        for case in cases {
            assert_eq!(read_text(case), None, "{case:?} must be refused");
        }
    }
}
