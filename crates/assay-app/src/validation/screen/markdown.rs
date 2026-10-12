//! What GitHub renders as code in reviewer text, by the `CommonMark` rules for fenced blocks
//! and code spans, so the screen can tell code from prose.
//!
//! Only what is unambiguously code is removed (wi-b5affa: refuse rather than model). Text that
//! is not plain (a `|`, whitespace other than space, tab and line feed, or a container line)
//! keeps everything; in plain text, an unclosed fence, an indented code block and every code
//! span of a paragraph in which some backtick run does not pair within its own line stay prose,
//! and so are screened: a false suspect costs a finding, a false exemption lets markup through.
//! Link destinations are read before code spans are paired (`links.rs`): a backtick inside a
//! link, which a span could claim, flags the text.

use std::borrow::Cow;

/// The most spaces a fence line may be indented by; four make an indented code line.
const MAX_FENCE_INDENT: usize = 3;

/// The shortest run of backticks or tildes that opens a fence.
const MIN_FENCE_RUN: usize = 3;

/// The info words of fences GitHub renders rather than shows as code (mermaid has click links,
/// math has `\href`), so their lines are prose (wi-b5affa).
const LIVE_FENCES: [&str; 5] = ["mermaid", "math", "geojson", "topojson", "stl"];

/// The most digits an ordered list marker may have (`CommonMark`).
const MAX_ORDERED_DIGITS: usize = 9;

/// Whether `text` is plain: no `|` (a table GitHub splits into cells before pairing
/// backticks), no whitespace but ASCII space, tab and line feed (a lone CR, NBSP and the like
/// break or join lines differently for GitHub), and no line beginning with a container marker
/// (a container can end a fence or paragraph GitHub opened inside it). Only plain text has
/// anything removed as code (wi-b5affa).
fn is_plain(text: &str) -> bool {
    !text.contains('|')
        && !text
            .chars()
            .any(|c| c.is_whitespace() && !matches!(c, ' ' | '\t' | '\n'))
        && !text.lines().any(|line| container_marker(line).is_some())
}

/// `text` without its fenced blocks and code spans, line by line; `text` as written if it is
/// not plain ([`is_plain`]).
///
/// Code spans are removed paragraph by paragraph, and only from a paragraph every backtick run
/// of which pairs within its own line: GitHub pairs runs across the lines of a paragraph.
///
/// Cost: every line is scanned once for fences. Within a line, each backtick run that opens a
/// span is matched by scanning forward for a run of the same length, so a line costs at most
/// its length times its number of runs. Fields are capped by the schema (1200 characters at
/// most), which bounds that product.
pub(super) fn prose(text: &str) -> Cow<'_, str> {
    if !is_plain(text) {
        return Cow::Borrowed(text);
    }
    let mut prose = String::with_capacity(text.len());
    let mut paragraph: Vec<&str> = Vec::new();
    // The open fence and its lines, opening line included, until a closing line arrives.
    let mut open: Option<(Fence, Vec<&str>)> = None;
    for line in text.lines() {
        open = match open {
            Some((fence, lines)) if fence.closed_by(line) => {
                // A fence GitHub renders live is closed as a block, but its lines stay prose.
                if fence.live {
                    for kept in lines.into_iter().chain([line]) {
                        prose.push_str(kept);
                        prose.push('\n');
                    }
                }
                None
            }
            Some((fence, mut lines)) => {
                lines.push(line);
                Some((fence, lines))
            }
            None => {
                if let Some(fence) = Fence::opened_by(line) {
                    flush(&mut paragraph, &mut prose);
                    Some((fence, vec![line]))
                } else {
                    add(line, &mut paragraph, &mut prose);
                    None
                }
            }
        };
    }
    // A fence never closed is ambiguous: CommonMark runs it to the end, a later edit may not.
    if let Some((_, lines)) = open {
        for line in lines {
            add(line, &mut paragraph, &mut prose);
        }
    }
    flush(&mut paragraph, &mut prose);
    Cow::Owned(prose)
}

/// Adds `line` to the open `paragraph`, or ends it at a blank line.
fn add<'t>(line: &'t str, paragraph: &mut Vec<&'t str>, prose: &mut String) {
    if line.trim_matches([' ', '\t']).is_empty() {
        flush(paragraph, prose);
        prose.push('\n');
    } else {
        paragraph.push(line);
    }
}

/// Appends `paragraph` to `prose`, without its code spans if every line's backtick runs pair
/// within that line, else as written; then empties it.
fn flush(paragraph: &mut Vec<&str>, prose: &mut String) {
    let stripped: Option<Vec<String>> = paragraph.iter().map(|line| without_spans(line)).collect();
    match stripped {
        Some(lines) => {
            for line in lines {
                prose.push_str(&line);
                prose.push('\n');
            }
        }
        None => {
            for line in paragraph.iter() {
                prose.push_str(line);
                prose.push('\n');
            }
        }
    }
    paragraph.clear();
}

/// The rest of `line` after one container marker, if it starts with one: a `>`, or a `-`, `*`
/// or `+` bullet, or an ordered marker (`1.`, `1)`), each after any indentation; a list marker
/// must be followed by a space, a tab or the end of the line.
pub(super) fn container_marker(line: &str) -> Option<&str> {
    let body = line.trim_start_matches([' ', '\t']);
    if let Some(rest) = body.strip_prefix('>') {
        return Some(rest);
    }
    let digits = body.chars().take_while(char::is_ascii_digit).count();
    let rest = if digits == 0 {
        body.strip_prefix(['-', '*', '+'])?
    } else if digits <= MAX_ORDERED_DIGITS {
        body.get(digits..)?.strip_prefix(['.', ')'])?
    } else {
        return None;
    };
    (rest.is_empty() || rest.starts_with([' ', '\t'])).then_some(rest)
}

/// An open fenced block: its character, the length of its opening run, and whether GitHub
/// renders it live.
#[derive(Debug, Clone, Copy)]
struct Fence {
    marker: char,
    run: usize,
    live: bool,
}

impl Fence {
    /// The fence `line` opens: at most three spaces, then three or more backticks or tildes;
    /// a backtick fence's info string may not hold a backtick. It is live if its info string's
    /// first word is one of [`LIVE_FENCES`], ASCII case-insensitive.
    fn opened_by(line: &str) -> Option<Self> {
        let body = unindented(line)?;
        let marker = body.chars().next().filter(|&c| c == '`' || c == '~')?;
        let run = run_length(body, marker);
        let info = body.get(run..)?;
        let opens = run >= MIN_FENCE_RUN && !(marker == '`' && info.contains('`'));
        let rendered = info.split_whitespace().next().is_some_and(|word| {
            LIVE_FENCES
                .iter()
                .any(|name| name.eq_ignore_ascii_case(word))
        });
        opens.then_some(Self {
            marker,
            run,
            live: rendered,
        })
    }

    /// Whether `line` closes this fence: at most three spaces, a run of its character at least
    /// as long as the opening one, then only spaces or tabs.
    fn closed_by(self, line: &str) -> bool {
        unindented(line).is_some_and(|body| {
            let run = run_length(body, self.marker);
            run >= self.run
                && body
                    .get(run..)
                    .is_some_and(|rest| rest.trim_matches([' ', '\t']).is_empty())
        })
    }
}

/// `line` without its leading spaces, if there are at most [`MAX_FENCE_INDENT`]. A tab is not
/// skipped, so a line indented with one opens and closes nothing.
fn unindented(line: &str) -> Option<&str> {
    let body = line.trim_start_matches(' ');
    let indent = line.len().checked_sub(body.len())?;
    (indent <= MAX_FENCE_INDENT).then_some(body)
}

/// How many times `marker`, an ASCII character, starts `text`; also its length in bytes.
fn run_length(text: &str, marker: char) -> usize {
    text.chars().take_while(|&c| c == marker).count()
}

/// `line` without its code spans, each replaced by a space; `None` if any backtick run on it
/// does not pair: one preceded by a backslash, or one with no later run of the same length.
fn without_spans(line: &str) -> Option<String> {
    let mut prose = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find('`') {
        let (before, from) = rest.split_at_checked(open)?;
        if before.ends_with('\\') {
            return None;
        }
        let run = run_length(from, '`');
        let after = from.get(run..)?;
        let end = closing(after, run)?.checked_add(run)?;
        prose.push_str(before);
        prose.push(' ');
        rest = after.get(end..)?;
    }
    prose.push_str(rest);
    Some(prose)
}

/// The byte offset in `text` of the first backtick run exactly `run` long.
fn closing(text: &str, run: usize) -> Option<usize> {
    let mut offset = 0_usize;
    loop {
        let start = offset.checked_add(text.get(offset..)?.find('`')?)?;
        let length = run_length(text.get(start..)?, '`');
        if length == run {
            return Some(start);
        }
        offset = start.checked_add(length)?;
    }
}

#[cfg(test)]
mod tests {
    use super::{container_marker, prose};

    #[test]
    fn removes_a_span_and_keeps_the_text_around_it() {
        assert_eq!(prose("a `b` c"), "a   c\n");
    }

    #[test]
    fn closes_a_span_only_at_a_run_of_the_same_length() {
        assert_eq!(prose("``a`b`` c"), "  c\n");
    }

    #[test]
    fn keeps_an_unmatched_or_escaped_opener_as_text() {
        assert_eq!([prose("`` a"), prose("\\`a` b")], ["`` a\n", "\\`a` b\n"]);
    }

    #[test]
    fn recognises_container_markers() {
        let lines = [
            "> x",
            "  - x",
            "* x",
            "+",
            "1. x",
            "12) x",
            "-x",
            "1.x",
            "1234567890. x",
            "x",
        ];

        assert_eq!(
            lines.map(|line| container_marker(line).is_some()),
            [
                true, true, true, true, true, true, false, false, false, false
            ]
        );
    }

    #[test]
    fn keeps_a_fence_as_prose_in_text_with_a_container() {
        assert_eq!(prose("> a\n```\ny\n```"), "> a\n```\ny\n```");
    }

    #[test]
    fn keeps_a_paragraph_as_written_when_a_run_pairs_only_across_lines() {
        assert_eq!(
            [prose("`a` `b\nc`"), prose("`a` b\n\n`c")],
            ["`a` `b\nc`\n", "  b\n\n`c\n"]
        );
    }

    #[test]
    fn keeps_a_live_fence_as_prose_and_closes_it_as_a_block() {
        assert_eq!(
            prose("```Mermaid x\na\n```\nb\n```\nc\n```"),
            "```Mermaid x\na\n```\nb\n"
        );
    }

    #[test]
    fn removes_a_closed_fence_and_keeps_an_unclosed_one() {
        assert_eq!(
            [prose("x\n```\ny\n```\nz"), prose("x\n```\ny")],
            ["x\nz\n", "x\n```\ny\n"]
        );
    }
}
