//! Reads back an inline comment's fingerprint, class, title and the rest of its body exactly as
//! [`super::body`] writes them, for `assay status`. The text is returned as published: escaped
//! and defanged for GitHub, never unescaped back into model text (T8).
//!
//! Only the marker is required. A header or title that is not exactly the written shape gives
//! `None` for that part rather than a guess (wi-b5affa): a title holding a blank line splits its
//! paragraph and is not read, and neither is the rest after it.

use assay_domain::{Class, Fingerprint};

use super::posted_fingerprint;

/// What an inline comment published about one finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PublishedComment<'a> {
    /// The fingerprint its marker names.
    pub(crate) fp: Fingerprint,
    /// The class its header names.
    pub(crate) class: Option<Class>,
    /// The title, as published (escaped).
    pub(crate) title: Option<&'a str>,
    /// Everything after the title: body, labelled fields, code, footer, as published.
    pub(crate) rest: Option<&'a str>,
}

/// `body` read as [`super::body`] writes it; `None` when its first line is not a marker.
pub(crate) fn read_published(body: &str) -> Option<PublishedComment<'_>> {
    let fp = posted_fingerprint(body)?;
    let after_marker = body.split_once('\n').map(|(_, after)| after);
    // A marker alone has no header: an empty one names no class.
    let (header, after_header) = match after_marker.and_then(|after| after.split_once('\n')) {
        Some((header, after)) => (header, Some(after)),
        None => (after_marker.unwrap_or(""), None),
    };
    let class = header
        .split(" · ")
        .nth(1)
        .and_then(|word| Class::ALL.into_iter().find(|class| class.code() == word));
    let titled = after_header.and_then(read_title);
    Some(PublishedComment {
        fp,
        class,
        title: titled.map(|(title, _)| title),
        rest: titled.and_then(|(_, rest)| rest),
    })
}

/// The `**<title>**` paragraph after the header's blank line, and what follows it. Every `*` of
/// the escaped title is preceded by a backslash, so a paragraph ending in `**` ends there; a
/// title holding a blank line leaves a first paragraph that does not.
fn read_title(after_header: &str) -> Option<(&str, Option<&str>)> {
    let opened = after_header.strip_prefix("\n**")?;
    let (paragraph, rest) = match opened.split_once("\n\n") {
        Some((paragraph, rest)) => (paragraph, Some(rest)),
        None => (opened, None),
    };
    let title = paragraph.strip_suffix("**")?;
    (!title.is_empty()).then_some((title, rest))
}

#[cfg(test)]
mod tests {
    use assay_domain::{Class, Confidence, Severity};

    use super::{PublishedComment, read_published};
    use crate::config::CommitId;
    use crate::publish::findings::comment::body;
    use crate::publish::findings::fixture::valid;
    use crate::publish::sanitise::Repository;

    fn head() -> CommitId {
        "0123456789abcdef0123456789abcdef01234567"
            .parse()
            .expect("40 lowercase hex digits")
    }

    #[test]
    fn reads_back_a_written_comment() {
        let found = valid(0xab, Severity::Major, Some(Confidence::High)).expect("fixture");
        let repository = Repository::parse("owner/repo").expect("a valid repository");
        let written = body(&found, repository, &head());

        let read = read_published(&written).expect("a marker");

        assert_eq!(read.fp, found.fingerprint);
        assert_eq!(read.class, Some(Class::Logic));
        assert_eq!(read.title, Some("Event recorded twice"));
        assert_eq!(
            read.rest
                .map(|rest| rest.starts_with("The retry path records the event again\\.")),
            Some(true),
            "the rest starts with the escaped body: {:?}",
            read.rest
        );
    }

    #[test]
    fn a_title_with_markup_is_read_escaped() {
        let mut found = valid(0x51, Severity::Blocker, None).expect("fixture");
        found.finding.title = "[click](https://evil.example) **bold**".to_owned();
        let repository = Repository::parse("owner/repo").expect("a valid repository");
        let written = body(&found, repository, &head());

        let read = read_published(&written).expect("a marker");

        assert_eq!(
            read.title,
            Some("\\[click\\]\\(https\\[\\:\\]\\/\\/evil\\.example\\) \\*\\*bold\\*\\*")
        );
    }

    #[test]
    fn a_title_with_a_blank_line_is_not_read() {
        let mut found = valid(0x52, Severity::Minor, None).expect("fixture");
        found.finding.title = "first\n\nsecond".to_owned();
        let repository = Repository::parse("owner/repo").expect("a valid repository");
        let written = body(&found, repository, &head());

        let read = read_published(&written).expect("a marker");

        assert_eq!((read.title, read.rest), (None, None));
        assert_eq!(read.class, Some(Class::Logic), "the header still reads");
    }

    #[test]
    fn a_comment_without_a_marker_is_not_a_finding() {
        assert_eq!(read_published("<!-- assay-summary -->\n\nanything"), None);
    }

    #[test]
    fn a_marker_with_nothing_after_it_reads_only_the_fingerprint() {
        let read = read_published("<!-- assay-finding 00000000000000ab -->");

        assert_eq!(
            read,
            Some(PublishedComment {
                fp: "00000000000000ab".parse().expect("a fingerprint"),
                class: None,
                title: None,
                rest: None,
            })
        );
    }

    #[test]
    fn an_unknown_class_is_not_read() {
        let read = read_published(
            "<!-- assay-finding 00000000000000ab -->\n🟡 **major** · nonsense · defect\n\n**T**",
        )
        .expect("a marker");

        assert_eq!((read.class, read.title, read.rest), (None, Some("T"), None));
    }
}
