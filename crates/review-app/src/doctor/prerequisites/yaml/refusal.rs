//! What the workflow reader refuses, and the character check it makes before reading lines.

use std::fmt;

/// Why a workflow file was refused, and on which line (1-based).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("line {line}: {reason}")]
pub struct YamlError {
    pub(super) line: usize,
    pub(super) reason: YamlRefusal,
}

impl YamlError {
    /// The line, counted from 1.
    #[must_use]
    pub const fn line(&self) -> usize {
        self.line
    }

    /// What the reader could not classify.
    #[must_use]
    pub const fn reason(&self) -> YamlRefusal {
        self.reason
    }
}

/// What the workflow reader refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum YamlRefusal {
    /// A tab outside a quoted scalar or block-scalar text.
    Tab,
    /// An anchor, alias, tag, directive, complex key, explicit block indentation or inline
    /// nested sequence.
    Indicator,
    /// A key that is not plain (`A-Z`, `a-z`, `0-9`, `_`, `-`, `.`) or quoted, a merge key
    /// included.
    Key,
    /// A flow mapping other than `{}`.
    FlowMapping,
    /// A flow sequence that is unterminated on its line, nests a collection or has an empty
    /// item.
    FlowSequence,
    /// A key repeated in one mapping.
    RepeatedKey,
    /// A line indented where nothing can continue: a continued scalar, or a stray item.
    Indentation,
    /// A second document, a document end or a directive.
    Document,
    /// A plain scalar holding `: ` or ending in `:`.
    Plain,
    /// A quoted scalar that does not end on its line, holds a backslash escape or is followed
    /// by more than a comment.
    Quote,
    /// Collections nested more than 32 levels deep.
    Depth,
    /// A character YAML reads as a line break (a carriage return not before a line feed,
    /// U+0085, U+2028, U+2029), a byte order mark, or a control character other than a line
    /// feed or a tab.
    Character,
}

impl fmt::Display for YamlRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Tab => "a tab outside quoted or block-scalar text",
            Self::Indicator => "an anchor, alias, tag or other unsupported indicator",
            Self::Key => "a key that is neither plain nor quoted",
            Self::FlowMapping => "a flow mapping",
            Self::FlowSequence => "a flow sequence that is not one line of scalars",
            Self::RepeatedKey => "a key repeated in one mapping",
            Self::Indentation => "unexpected indentation",
            Self::Document => "more than one document",
            Self::Plain => "a plain scalar holding a mapping indicator",
            Self::Quote => "a quoted scalar that is not one line without escapes",
            Self::Depth => "nesting deeper than the reader accepts",
            Self::Character => "a line break other than a line feed, or a control character",
        })
    }
}

/// Refuses, before `text` is split into lines, every character YAML reads as a line break
/// other than `\n` and `\r\n` (`str::lines` splits on those alone, so any other would hide
/// what follows it on the same line), a byte order mark, and every control character but `\n`
/// and `\t` (tabs are refused where they matter, by the reader).
pub(super) fn accepted_characters(text: &str) -> Result<(), YamlError> {
    let mut line = 1_usize;
    let mut chars = text.chars().peekable();
    while let Some(next) = chars.next() {
        let refused = match next {
            '\n' => {
                line = line.saturating_add(1);
                false
            }
            '\t' => false,
            '\r' => chars.peek() != Some(&'\n'),
            '\u{85}' | '\u{2028}' | '\u{2029}' | '\u{feff}' => true,
            _ => next.is_control(),
        };
        if refused {
            return Err(YamlError {
                line,
                reason: YamlRefusal::Character,
            });
        }
    }
    Ok(())
}
