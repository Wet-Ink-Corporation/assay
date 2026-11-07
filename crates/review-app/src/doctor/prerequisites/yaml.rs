//! A fail-closed reader for the block-YAML subset workflow files are written in (05 §7, #75).
//!
//! No YAML parser is a pre-approved dependency, and doctor needs only a few facts from a
//! workflow (its triggers, its `uses`, its jobs' `if`, `permissions` and `environment`). This
//! reader accepts block mappings and sequences of plain, quoted, flow-sequence and block
//! scalars, and refuses everything it cannot classify exactly (wi-b5affa): a line break other
//! than `\n` or `\r\n`, a byte order mark or a control character other than a tab (all before
//! the text is split into lines), a tab outside quoted and block-scalar text, an anchor, alias
//! or tag, a merge or complex key, a flow mapping other than `{}`, a flow sequence over
//! several lines or with nested collections, a plain or quoted scalar continued on the next
//! line, an escape in a double-quoted scalar, a second document, a repeated key and nesting
//! deeper than [`MAX_DEPTH`]. A refused workflow fails the check that reads it. The tree is
//! in `yaml/node.rs`, the refusals in `yaml/refusal.rs` and the one-line scalar readers in
//! `yaml/scalar.rs`.

mod node;
mod refusal;
mod scalar;

pub(crate) use node::Node;
pub use refusal::{YamlError, YamlRefusal};

use refusal::accepted_characters;
use scalar::{flow_sequence, is_indicator, key_of, plain, quoted, untabbed, without_comment};

/// The deepest nesting of collections read; deeper is [`YamlRefusal::Depth`] (H-13).
pub(crate) const MAX_DEPTH: usize = 32;

/// Reads `text` as the YAML subset described in the module docs.
///
/// # Errors
///
/// [`YamlError`] for the first line the reader cannot classify.
pub(crate) fn read(text: &str) -> Result<Node, YamlError> {
    accepted_characters(text)?;
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
                if raw.contains('\t') {
                    return Err(YamlError {
                        line: index.saturating_add(1),
                        reason: YamlRefusal::Tab,
                    });
                }
                self.next = index.saturating_add(1);
                continue;
            }
            // Trailing tabs stay, for the scalar readers to refuse.
            let line = Line {
                index,
                indent: indent_of(raw),
                body: raw.trim_start_matches(' ').trim_end_matches(' '),
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
            untabbed(rest).map_err(refuse)?;
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
            '|' | '>' => {
                let header = untabbed(rest).map_err(refuse)?;
                self.block_scalar(line, header, indent)
            }
            '[' => flow_sequence(rest).map_err(refuse),
            '{' if without_comment(untabbed(rest).map_err(refuse)?) == "{}" => {
                Ok(Node::Map(Vec::new()))
            }
            '{' => Err(refuse(YamlRefusal::FlowMapping)),
            '\'' | '"' => {
                let (text, after) = quoted(rest).map_err(refuse)?;
                if without_comment(untabbed(after).map_err(refuse)?).is_empty() {
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

#[cfg(test)]
mod tests;
