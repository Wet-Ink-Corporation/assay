//! The one-line pieces of the workflow reader: keys, plain, quoted and flow-sequence scalars,
//! comments and indicators. A tab is accepted only inside a quoted scalar: YAML reads a tab
//! after `:` as a value indicator and before `#` as the start of a comment, and this reader
//! does neither, so it refuses every other tab rather than misread one ([`YamlRefusal::Tab`]).

use super::{Node, YamlRefusal};

/// `text`, refused when it holds a tab.
pub(super) fn untabbed(text: &str) -> Result<&str, YamlRefusal> {
    if text.contains('\t') {
        Err(YamlRefusal::Tab)
    } else {
        Ok(text)
    }
}

/// Whether a value starts with an indicator the reader does not support.
pub(super) fn is_indicator(rest: &str) -> bool {
    rest.starts_with(['&', '*', '!', '%', '@', '`'])
        || rest == "-"
        || rest.starts_with("- ")
        || rest == "?"
        || rest.starts_with("? ")
}

/// `text` up to a comment (a `#` at its start or after a space), without trailing spaces.
pub(super) fn without_comment(text: &str) -> &str {
    let end = if text.starts_with('#') {
        0
    } else {
        text.find(" #").unwrap_or(text.len())
    };
    text.get(..end).unwrap_or(text).trim_end_matches(' ')
}

/// A plain scalar, refused when it holds a tab or a mapping indicator.
pub(super) fn plain(rest: &str) -> Result<String, YamlRefusal> {
    let text = without_comment(untabbed(rest)?);
    if text.contains(": ") || text.ends_with(':') {
        return Err(YamlRefusal::Plain);
    }
    Ok(text.to_owned())
}

/// The key `body` starts with and the rest after `: `, `None` when it is not a mapping entry.
pub(super) fn key_of(body: &str) -> Result<Option<(String, &str)>, YamlRefusal> {
    if body.starts_with(['\'', '"']) {
        let (key, after) = quoted(body)?;
        let Some(rest) = after.strip_prefix(':') else {
            return Ok(None);
        };
        if rest.starts_with('\t') {
            return Err(YamlRefusal::Tab);
        }
        return Ok(
            (rest.is_empty() || rest.starts_with(' ')).then(|| (key, rest.trim_start_matches(' ')))
        );
    }
    let comment = body.find(" #").unwrap_or(body.len());
    // A `:` is a mapping indicator before a space, a tab or the end of the line; the reader
    // takes it before a space or the end and refuses it before a tab.
    let indicator = body.match_indices(':').find_map(|(at, _)| {
        let (key, rest) = body.split_at_checked(at)?;
        let rest = rest.strip_prefix(':')?;
        (rest.is_empty() || rest.starts_with([' ', '\t'])).then_some((key, rest))
    });
    let Some((key, rest)) = indicator.filter(|(key, _)| key.len() < comment) else {
        return Ok(None);
    };
    if key.contains('\t') || rest.starts_with('\t') {
        return Err(YamlRefusal::Tab);
    }
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
pub(super) fn quoted(text: &str) -> Result<(String, &str), YamlRefusal> {
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
        let after = text.get(at.saturating_add(1)..).ok_or(YamlRefusal::Quote)?;
        if quote == '\'' && after.starts_with('\'') {
            value.push('\'');
            chars.next();
            continue;
        }
        return Ok((value, after));
    }
    Err(YamlRefusal::Quote)
}

/// `text` without its leading spaces, refused when a tab follows them.
fn spaces(text: &str) -> Result<&str, YamlRefusal> {
    let text = text.trim_start_matches(' ');
    if text.starts_with('\t') {
        Err(YamlRefusal::Tab)
    } else {
        Ok(text)
    }
}

/// A one-line flow sequence of scalars, `[a, 'b']`.
pub(super) fn flow_sequence(rest: &str) -> Result<Node, YamlRefusal> {
    let inner = rest.strip_prefix('[').ok_or(YamlRefusal::FlowSequence)?;
    let mut items = Vec::new();
    let mut remaining = spaces(inner)?;
    if let Some(after) = remaining.strip_prefix(']') {
        return close(after, items);
    }
    loop {
        let (item, after) = flow_item(remaining)?;
        items.push(Node::Scalar(item));
        let after = spaces(after)?;
        if let Some(after) = after.strip_prefix(']') {
            return close(after, items);
        }
        remaining = spaces(after.strip_prefix(',').ok_or(YamlRefusal::FlowSequence)?)?;
    }
}

/// The sequence, when only a comment follows its `]`.
fn close(after: &str, items: Vec<Node>) -> Result<Node, YamlRefusal> {
    if without_comment(untabbed(after)?).is_empty() {
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
    let (item, after) = text
        .split_at_checked(end)
        .ok_or(YamlRefusal::FlowSequence)?;
    let item = untabbed(item)?.trim_end_matches(' ');
    if item.is_empty() || item.contains(['[', '{', '#']) || item.contains(": ") {
        return Err(YamlRefusal::FlowSequence);
    }
    if is_indicator(item) {
        return Err(YamlRefusal::Indicator);
    }
    Ok((item.to_owned(), after))
}
