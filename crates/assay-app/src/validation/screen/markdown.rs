//! What GitHub renders as code in reviewer text, by the `CommonMark` rules for fenced blocks
//! and code spans, so the screen can tell code from prose.
//!
//! Only what is unambiguously code is removed. An unclosed fence, a code span crossing a line
//! and an indented code block stay prose, and so are screened: a false suspect costs a finding,
//! a false exemption lets markup through.

/// The most spaces a fence line may be indented by; four make an indented code line.
const MAX_FENCE_INDENT: usize = 3;

/// The shortest run of backticks or tildes that opens a fence.
const MIN_FENCE_RUN: usize = 3;

/// `text` without its fenced blocks and code spans, line by line.
///
/// Cost: every line is scanned once for fences. Within a line, each backtick run that opens a
/// span is matched by scanning forward for a run of the same length, so a line costs at most
/// its length times its number of runs. Fields are capped by the schema (1200 characters at
/// most), which bounds that product.
pub(super) fn prose(text: &str) -> String {
    let mut prose = String::with_capacity(text.len());
    // The open fence and its lines, opening line included, until a closing line arrives.
    let mut open: Option<(Fence, Vec<&str>)> = None;
    for line in text.lines() {
        open = match open {
            Some((fence, _)) if fence.closed_by(line) => None,
            Some((fence, mut lines)) => {
                lines.push(line);
                Some((fence, lines))
            }
            None => {
                let fence = Fence::opened_by(line);
                if fence.is_none() {
                    push_line(line, &mut prose);
                }
                fence.map(|fence| (fence, vec![line]))
            }
        };
    }
    // A fence never closed is ambiguous: CommonMark runs it to the end, a later edit may not.
    if let Some((_, lines)) = open {
        for line in lines {
            push_line(line, &mut prose);
        }
    }
    prose
}

/// An open fenced block: its character and the length of its opening run.
#[derive(Debug, Clone, Copy)]
struct Fence {
    marker: char,
    run: usize,
}

impl Fence {
    /// The fence `line` opens: at most three spaces, then three or more backticks or tildes;
    /// a backtick fence's info string may not hold a backtick.
    fn opened_by(line: &str) -> Option<Self> {
        let body = unindented(line)?;
        let marker = body.chars().next().filter(|&c| c == '`' || c == '~')?;
        let run = run_length(body, marker);
        let info = body.get(run..)?;
        let opens = run >= MIN_FENCE_RUN && !(marker == '`' && info.contains('`'));
        opens.then_some(Self { marker, run })
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

/// Appends `line` to `prose` without its code spans, each replaced by a space.
///
/// A span opens at a backtick run not preceded by a backslash and closes at the next run of the
/// same length on the line; an opening run with no such closer is literal text.
fn push_line(line: &str, prose: &mut String) {
    let mut rest = line;
    while let Some(open) = rest.find('`') {
        let Some((before, from)) = rest.split_at_checked(open) else {
            break;
        };
        let run = run_length(from, '`');
        let Some((opener, after)) = from.split_at_checked(run) else {
            break;
        };
        prose.push_str(before);
        let span_end = if before.ends_with('\\') {
            None
        } else {
            closing(after, run).and_then(|close| close.checked_add(run))
        };
        if let Some(remaining) = span_end.and_then(|end| after.get(end..)) {
            prose.push(' ');
            rest = remaining;
        } else {
            prose.push_str(opener);
            rest = after;
        }
    }
    prose.push_str(rest);
    prose.push('\n');
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
    use super::prose;

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
    fn removes_a_closed_fence_and_keeps_an_unclosed_one() {
        assert_eq!(
            [prose("x\n```\ny\n```\nz"), prose("x\n```\ny")],
            ["x\nz\n", "x\n```\ny\n"]
        );
    }
}
