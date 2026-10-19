//! A fail-closed reader for the block-YAML subset workflow files are written in (05 §7, #75).
//!
//! No YAML parser is a pre-approved dependency, and doctor needs only a few facts from a
//! workflow (its triggers, its `uses`, its jobs' `if`, `permissions` and `environment`). This
//! reader accepts block mappings and sequences of plain, quoted, flow-sequence and block
//! scalars, and refuses everything it cannot classify exactly (wi-b5affa): a tab in the
//! indentation, an anchor, alias or tag, a merge or complex key, a flow mapping other than
//! `{}`, a flow sequence over several lines or with nested collections, a plain or quoted
//! scalar continued on the next line, an escape in a double-quoted scalar, a second document,
//! a repeated key and nesting deeper than [`MAX_DEPTH`]. A refused workflow fails the check
//! that reads it.

use std::fmt;

/// The deepest nesting of collections read; deeper is [`YamlRefusal::Depth`] (H-13).
pub(crate) const MAX_DEPTH: usize = 32;

/// A read document, as a tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Node {
    /// No value (`key:` with nothing under it), or an empty document.
    Null,
    /// A plain, single- or double-quoted scalar, unquoted.
    Scalar(String),
    /// A block scalar (`|` or `>`), its lines joined with `\n` without their indentation.
    Text(String),
    /// A sequence.
    Seq(Vec<Node>),
    /// A mapping, in file order, with no key twice.
    Map(Vec<(String, Node)>),
}

impl Node {
    /// The value of `key` when this is a mapping holding it.
    pub(crate) fn get(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Map(entries) => entries
                .iter()
                .find(|(candidate, _)| candidate == key)
                .map(|(_, value)| value),
            Self::Null | Self::Scalar(_) | Self::Text(_) | Self::Seq(_) => None,
        }
    }

    /// Takes the value of `key` out of this mapping, when it holds it.
    pub(crate) fn remove(&mut self, key: &str) -> Option<Self> {
        match self {
            Self::Map(entries) => {
                let at = entries.iter().position(|(candidate, _)| candidate == key)?;
                Some(entries.remove(at).1)
            }
            Self::Null | Self::Scalar(_) | Self::Text(_) | Self::Seq(_) => None,
        }
    }

    /// Whether `needle` appears in any key or scalar under this node.
    pub(crate) fn mentions(&self, needle: &str) -> bool {
        match self {
            Self::Null => false,
            Self::Scalar(text) | Self::Text(text) => text.contains(needle),
            Self::Seq(items) => items.iter().any(|item| item.mentions(needle)),
            Self::Map(entries) => entries
                .iter()
                .any(|(key, value)| key.contains(needle) || value.mentions(needle)),
        }
    }
}

/// Why a workflow file was refused, and on which line (1-based).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("line {line}: {reason}")]
pub struct YamlError {
    line: usize,
    reason: YamlRefusal,
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
    /// A tab in a line's indentation.
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
}

impl fmt::Display for YamlRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Tab => "a tab in the indentation",
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
        })
    }
}

/// Reads `text` as the YAML subset described in the module docs.
///
/// # Errors
///
/// [`YamlError`] for the first line the reader cannot classify.
pub(crate) fn read(text: &str) -> Result<Node, YamlError> {
    let mut reader = Reader {
        lines: text.lines().collect(),
        next: 0,
        started: false,
    };
    let Some(first) = reader.peek()? else {
        return Ok(Node::Null);
    };
    let node = reader.block(first, 0)?;
    match reader.peek()? {
        None => Ok(node),
        Some(line) => Err(line.refuse(YamlRefusal::Indentation)),
    }
}

/// One significant line: not blank and not only a comment.
#[derive(Debug, Clone, Copy)]
struct Line<'t> {
    /// Its index in the file.
    index: usize,
    /// Its leading spaces.
    indent: usize,
    /// The rest of it.
    body: &'t str,
}

impl Line<'_> {
    fn refuse(self, reason: YamlRefusal) -> YamlError {
        YamlError {
            line: self.index.saturating_add(1),
            reason,
        }
    }

    /// Whether the line is a sequence item.
    fn is_item(self) -> bool {
        is_item(self.body)
    }
}

fn is_item(body: &str) -> bool {
    body == "-" || body.starts_with("- ")
}

/// The leading spaces of `line`.
fn indent_of(line: &str) -> usize {
    line.len()
        .saturating_sub(line.trim_start_matches(' ').len())
}

struct Reader<'t> {
    lines: Vec<&'t str>,
    /// The index of the next line not yet consumed.
    next: usize,
    /// Whether a significant line or a leading `---` has been seen.
    started: bool,
}

impl<'t> Reader<'t> {
    /// The next significant line, not consumed. A leading `---` is skipped once.
    fn peek(&mut self) -> Result<Option<Line<'t>>, YamlError> {
        while let Some(&raw) = self.lines.get(self.next) {
            let index = self.next;
            let trimmed = raw.trim_start_matches([' ', '\t']);
            if trimmed.is_empty() || trimmed.starts_with('#') {
                self.next = index.saturating_add(1);
                continue;
            }
            let line = Line {
                index,
                indent: indent_of(raw),
                body: raw.trim_start_matches(' ').trim_end(),
            };
            if raw.len().saturating_sub(trimmed.len()) != line.indent {
                return Err(line.refuse(YamlRefusal::Tab));
            }
            let marker = |mark: &str| {
                line.body
                    .strip_prefix(mark)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
            };
            if line.indent == 0 && (marker("---") || marker("...") || line.body.starts_with('%')) {
                if !self.started && line.body == "---" {
                    self.started = true;
                    self.next = index.saturating_add(1);
                    continue;
                }
                return Err(line.refuse(YamlRefusal::Document));
            }
            self.started = true;
            return Ok(Some(line));
        }
        Ok(None)
    }

    /// Consumes `line`.
    fn take(&mut self, line: Line<'t>) {
        self.next = line.index.saturating_add(1);
    }

    /// The collection that starts at `first`, at nesting `depth`.
    fn block(&mut self, first: Line<'t>, depth: usize) -> Result<Node, YamlError> {
        if depth >= MAX_DEPTH {
            return Err(first.refuse(YamlRefusal::Depth));
        }
        if first.is_item() {
            self.sequence(first.indent, depth)
        } else {
            self.mapping(first.indent, depth, Vec::new())
        }
    }

    /// The mapping at `indent`, continuing `entries`.
    fn mapping(
        &mut self,
        indent: usize,
        depth: usize,
        mut entries: Vec<(String, Node)>,
    ) -> Result<Node, YamlError> {
        while let Some(line) = self.peek()? {
            if line.indent < indent {
                break;
            }
            if line.indent > indent || line.is_item() {
                return Err(line.refuse(YamlRefusal::Indentation));
            }
            self.take(line);
            let (key, rest) = key_of(line.body)
                .map_err(|reason| line.refuse(reason))?
                .ok_or_else(|| line.refuse(YamlRefusal::Plain))?;
            let start = Start {
                line,
                rest,
                after_key: true,
            };
            let value = self.value(start, At { indent, depth })?;
            insert(&mut entries, line, key, value)?;
        }
        Ok(Node::Map(entries))
    }

    /// The sequence at `indent`.
    fn sequence(&mut self, indent: usize, depth: usize) -> Result<Node, YamlError> {
        let mut items = Vec::new();
        while let Some(line) = self.peek()? {
            if line.indent < indent || (line.indent == indent && !line.is_item()) {
                break;
            }
            if line.indent > indent {
                return Err(line.refuse(YamlRefusal::Indentation));
            }
            self.take(line);
            items.push(self.item(line, indent, depth)?);
        }
        Ok(Node::Seq(items))
    }

    /// One sequence item: a nested block, a mapping starting on the item's line, or a value.
    fn item(&mut self, line: Line<'t>, indent: usize, depth: usize) -> Result<Node, YamlError> {
        let Some(after_dash) = line.body.strip_prefix('-') else {
            return Err(line.refuse(YamlRefusal::Indentation));
        };
        let content = after_dash.trim_start_matches(' ');
        if is_item(content) {
            return Err(line.refuse(YamlRefusal::Indicator));
        }
        let Some((key, rest)) = key_of(content).map_err(|reason| line.refuse(reason))? else {
            let start = Start {
                line,
                rest: content,
                after_key: false,
            };
            return self.value(start, At { indent, depth });
        };
        if depth.saturating_add(1) >= MAX_DEPTH {
            return Err(line.refuse(YamlRefusal::Depth));
        }
        let spaces = after_dash.len().saturating_sub(content.len());
        let at = At {
            indent: indent.saturating_add(1).saturating_add(spaces),
            depth: depth.saturating_add(1),
        };
        let start = Start {
            line,
            rest,
            after_key: true,
        };
        let value = self.value(start, at)?;
        let mut entries = Vec::new();
        insert(&mut entries, line, key, value)?;
        self.mapping(at.indent, at.depth, entries)
    }

    /// The value `start` begins, whose key or item sits at `at`. An empty `rest` takes the
    /// block below it, which may be a sequence at the key's own indentation.
    fn value(&mut self, start: Start<'t>, at: At) -> Result<Node, YamlError> {
        let Start {
            line,
            rest,
            after_key,
        } = start;
        let At { indent, depth } = at;
        let refuse = |reason| line.refuse(reason);
        let Some(first) = rest.chars().next().filter(|&first| first != '#') else {
            return match self.peek()? {
                Some(next)
                    if next.indent > indent
                        || (after_key && next.indent == indent && next.is_item()) =>
                {
                    self.block(next, depth.saturating_add(1))
                }
                Some(_) | None => Ok(Node::Null),
            };
        };
        match first {
            '|' | '>' => self.block_scalar(line, rest, indent),
            '[' => flow_sequence(rest).map_err(refuse),
            '{' if without_comment(rest) == "{}" => Ok(Node::Map(Vec::new())),
            '{' => Err(refuse(YamlRefusal::FlowMapping)),
            '\'' | '"' => {
                let (text, after) = quoted(rest).map_err(refuse)?;
                if without_comment(after).is_empty() {
                    Ok(Node::Scalar(text))
                } else {
                    Err(refuse(YamlRefusal::Quote))
                }
            }
            _ if is_indicator(rest) => Err(refuse(YamlRefusal::Indicator)),
            _ => plain(rest).map(Node::Scalar).map_err(refuse),
        }
    }

    /// A block scalar under `line`: every following line that is blank or indented past
    /// `indent`, without the first content line's indentation.
    fn block_scalar(
        &mut self,
        line: Line<'t>,
        header: &str,
        indent: usize,
    ) -> Result<Node, YamlError> {
        let header = without_comment(header);
        if !matches!(header, "|" | "|-" | "|+" | ">" | ">-" | ">+") {
            return Err(line.refuse(YamlRefusal::Indicator));
        }
        let mut content: Vec<&str> = Vec::new();
        let mut body_indent = None;
        while let Some(&raw) = self.lines.get(self.next) {
            if raw.trim().is_empty() {
                content.push("");
            } else {
                let raw_indent = indent_of(raw);
                if raw_indent <= indent {
                    break;
                }
                let at = *body_indent.get_or_insert(raw_indent);
                // The first `at` bytes are spaces, so the split is on a character boundary.
                let Some((_, text)) = raw.split_at_checked(at).filter(|_| raw_indent >= at) else {
                    return Err(YamlError {
                        line: self.next.saturating_add(1),
                        reason: YamlRefusal::Indentation,
                    });
                };
                content.push(text);
            }
            self.next = self.next.saturating_add(1);
        }
        while content.last() == Some(&"") {
            content.pop();
        }
        Ok(Node::Text(content.join("\n")))
    }
}

/// Where a value's key or item sits: its indentation and its nesting depth.
#[derive(Debug, Clone, Copy)]
struct At {
    indent: usize,
    depth: usize,
}

/// Where a value starts: its line, the text after its key or dash, and whether a key (rather
/// than a sequence dash) introduced it.
#[derive(Debug, Clone, Copy)]
struct Start<'t> {
    line: Line<'t>,
    rest: &'t str,
    after_key: bool,
}

/// Adds `key` and `value` to `entries`, refusing a key `entries` already holds.
fn insert(
    entries: &mut Vec<(String, Node)>,
    line: Line<'_>,
    key: String,
    value: Node,
) -> Result<(), YamlError> {
    if entries.iter().any(|(seen, _)| *seen == key) {
        return Err(line.refuse(YamlRefusal::RepeatedKey));
    }
    entries.push((key, value));
    Ok(())
}

/// Whether a value starts with an indicator the reader does not support.
fn is_indicator(rest: &str) -> bool {
    rest.starts_with(['&', '*', '!', '%', '@', '`'])
        || rest == "-"
        || rest.starts_with("- ")
        || rest == "?"
        || rest.starts_with("? ")
}

/// `text` up to a comment (a `#` at its start or after a space), without trailing space.
fn without_comment(text: &str) -> &str {
    let end = if text.starts_with('#') {
        0
    } else {
        text.find(" #").unwrap_or(text.len())
    };
    text.get(..end).unwrap_or(text).trim_end()
}

/// A plain scalar, refused when it holds a mapping indicator.
fn plain(rest: &str) -> Result<String, YamlRefusal> {
    let text = without_comment(rest);
    if text.contains(": ") || text.ends_with(':') {
        return Err(YamlRefusal::Plain);
    }
    Ok(text.to_owned())
}

/// The key `body` starts with and the rest after `: `, `None` when it is not a mapping entry.
fn key_of(body: &str) -> Result<Option<(String, &str)>, YamlRefusal> {
    if body.starts_with(['\'', '"']) {
        let (key, after) = quoted(body)?;
        return Ok(after
            .strip_prefix(':')
            .filter(|rest| rest.is_empty() || rest.starts_with(' '))
            .map(|rest| (key, rest.trim_start_matches(' '))));
    }
    let comment = body.find(" #").unwrap_or(body.len());
    // A `:` is a mapping indicator only before a space or at the end of the line.
    let indicator = body.match_indices(':').find_map(|(at, _)| {
        let (key, rest) = body.split_at_checked(at)?;
        let rest = rest.strip_prefix(':')?;
        (rest.is_empty() || rest.starts_with(' ')).then_some((key, rest))
    });
    let Some((key, rest)) = indicator.filter(|(key, _)| key.len() < comment) else {
        return Ok(None);
    };
    let plain_key = !key.is_empty()
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
    if !plain_key {
        return Err(YamlRefusal::Key);
    }
    Ok(Some((key.to_owned(), rest.trim_start_matches(' '))))
}

/// The quoted scalar `text` starts with, unquoted, and what follows its closing quote.
fn quoted(text: &str) -> Result<(String, &str), YamlRefusal> {
    let mut chars = text.char_indices();
    let Some((_, quote)) = chars.next() else {
        return Err(YamlRefusal::Quote);
    };
    let mut value = String::new();
    while let Some((at, next)) = chars.next() {
        if next == '\\' && quote == '"' {
            return Err(YamlRefusal::Quote);
        }
        if next != quote {
            value.push(next);
            continue;
        }
        let after = text.get(at.saturating_add(1)..).unwrap_or_default();
        if quote == '\'' && after.starts_with('\'') {
            value.push('\'');
            chars.next();
            continue;
        }
        return Ok((value, after));
    }
    Err(YamlRefusal::Quote)
}

/// A one-line flow sequence of scalars, `[a, 'b']`.
fn flow_sequence(rest: &str) -> Result<Node, YamlRefusal> {
    let inner = rest.strip_prefix('[').ok_or(YamlRefusal::FlowSequence)?;
    let mut items = Vec::new();
    let mut remaining = inner.trim_start();
    if let Some(after) = remaining.strip_prefix(']') {
        return close(after, items);
    }
    loop {
        let (item, after) = flow_item(remaining)?;
        items.push(Node::Scalar(item));
        let after = after.trim_start();
        if let Some(after) = after.strip_prefix(']') {
            return close(after, items);
        }
        remaining = after
            .strip_prefix(',')
            .ok_or(YamlRefusal::FlowSequence)?
            .trim_start();
    }
}

/// The sequence, when only a comment follows its `]`.
fn close(after: &str, items: Vec<Node>) -> Result<Node, YamlRefusal> {
    if without_comment(after).is_empty() {
        Ok(Node::Seq(items))
    } else {
        Err(YamlRefusal::FlowSequence)
    }
}

/// One item of a flow sequence and what follows it.
fn flow_item(text: &str) -> Result<(String, &str), YamlRefusal> {
    if text.starts_with(['\'', '"']) {
        return quoted(text);
    }
    let end = text.find([',', ']']).ok_or(YamlRefusal::FlowSequence)?;
    let item = text.get(..end).unwrap_or_default().trim_end();
    if item.is_empty() || item.contains(['[', '{', '#']) || item.contains(": ") {
        return Err(YamlRefusal::FlowSequence);
    }
    if is_indicator(item) {
        return Err(YamlRefusal::Indicator);
    }
    Ok((item.to_owned(), text.get(end..).unwrap_or_default()))
}

#[cfg(test)]
mod tests;
