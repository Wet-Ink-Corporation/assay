//! decide-only's replies (#61 DO-9, D10): a closed sentence under a marker keyed by the comment
//! id, never the comment's text, and never itself a command.

use std::num::NonZeroU64;

use assay_app::{CommandRejected, CommentId, Ineligible, Parsed, Reply, parse_command};

fn comment(id: u64) -> CommentId {
    CommentId(NonZeroU64::new(id).expect("non-zero"))
}

/// One of each reply.
fn every_reply() -> [Reply; 9] {
    [
        Reply::Rejected(CommandRejected::Unknown),
        Reply::Rejected(CommandRejected::Malformed),
        Reply::Rejected(CommandRejected::Ineligible(Ineligible::NotPrivileged)),
        Reply::RateLimited,
        Reply::RetryUnavailable,
        Reply::NoEvidence,
        Reply::RerunNeeded,
        Reply::HeadMoved,
        Reply::NotRecorded,
    ]
}

#[test]
fn a_reply_body_is_its_marker_line_then_its_fixed_sentence() {
    let body = Reply::NoEvidence.body(comment(42));

    assert_eq!(Reply::marker(comment(42)), "<!-- assay-reply 42 -->");
    assert_eq!(
        body,
        "<!-- assay-reply 42 -->\nassay did not act on this command: this PR has no completed \
         assay run on its head to decide again."
    );
}

#[test]
fn a_rejected_command_reply_names_only_its_closed_reason() {
    let body = Reply::Rejected(CommandRejected::Ineligible(Ineligible::Author)).body(comment(7));

    assert_eq!(
        body,
        "<!-- assay-reply 7 -->\nassay did not record this command: the author is not \
         eligible: the PR author."
    );
}

#[test]
fn no_reply_body_reads_as_a_command() {
    for reply in every_reply() {
        let body = reply.body(comment(9));

        assert_eq!(parse_command(&body), Parsed::NotACommand, "{body}");
        assert_eq!(
            body.split_once('\n').map(|(first, _)| first),
            Some("<!-- assay-reply 9 -->"),
            "{reply:?}"
        );
    }
}
