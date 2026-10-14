//! The publication-side backstop for model-authored text (04 §6 "Sanitisation", #175).
//!
//! Policy wi-b5affa, "refuse rather than model": nothing here reads Markdown. Model prose is
//! made inert by construction instead. Every ASCII punctuation character is backslash-escaped,
//! which Markdown allows for every one of them, so no inline or block construct (link, image,
//! autolink, raw HTML, code span, fence, emphasis, table, heading, quote, list) can start. What
//! GitHub turns live without punctuation is defanged by substitution: `@` (mentions, email
//! autolinks) and `#` (cross-repository references) become their fullwidth forms, `://` and
//! `www.` (extended autolinks) are written `[:]//` and `www[.]`. Lines lose their leading
//! whitespace (no indented code) and control and format characters become U+FFFD. The only
//! live links are the permalinks [`permalink`] builds and the only fences [`code_block`]'s,
//! whose info string comes from the closed [`Info`] list, which has no `suggestion`.

use std::num::NonZeroU64;

use super::render::is_hidden;
use crate::config::CommitId;

/// What a clipped text ends with.
const ELLIPSIS: char = '…';

/// The most lines of code a fenced block shows (H-13).
pub(super) const MAX_CODE_LINES: usize = 20;

/// The most characters of code a fenced block shows (H-13).
pub(super) const MAX_CODE_CHARS: usize = 1000;

/// The longest path, in bytes, a permalink is built for; a longer one is shown as plain text.
pub(super) const MAX_PATH_BYTES: usize = 1024;

/// The longest owner or repository name GitHub allows.
const MAX_NAME_BYTES: usize = 100;

/// `text` as inert Markdown: at most `max_chars` characters of it (then `…`), escaped and
/// defanged as the module documentation describes. Runs of blank lines become one.
pub(super) fn prose(text: &str, max_chars: usize) -> String {
    let (clipped, cut) = clip(text, max_chars);
    let mut out = String::with_capacity(clipped.len().saturating_mul(2));
    let mut blank = false;
    for line in clipped.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line).trim();
        if line.is_empty() {
            blank = !out.is_empty();
            continue;
        }
        if !out.is_empty() {
            out.push_str(if blank { "\n\n" } else { "\n" });
        }
        blank = false;
        escape_line_into(line, &mut out);
    }
    if cut {
        out.push(ELLIPSIS);
    }
    out
}

/// The first `max_chars` characters of `text`, and whether anything was cut.
fn clip(text: &str, max_chars: usize) -> (&str, bool) {
    match text
        .char_indices()
        .nth(max_chars)
        .and_then(|(at, _)| text.split_at_checked(at))
    {
        Some((kept, _)) => (kept, true),
        None => (text, false),
    }
}

/// Appends `line` (no line feed in it) escaped and defanged.
fn escape_line_into(line: &str, out: &mut String) {
    let mut chars = line.chars();
    loop {
        let rest = chars.as_str();
        if let Some(after) = rest.strip_prefix("://") {
            out.push_str(r"\[\:\]\/\/");
            chars = after.chars();
            continue;
        }
        if let Some((www, after)) = rest.split_at_checked(4)
            && www.eq_ignore_ascii_case("www.")
        {
            out.extend(www.chars().take(3));
            out.push_str(r"\[\.\]");
            chars = after.chars();
            continue;
        }
        let Some(c) = chars.next() else {
            break;
        };
        match c {
            '@' => out.push('\u{ff20}'),
            '#' => out.push('\u{ff03}'),
            c if c.is_ascii_punctuation() => {
                out.push('\\');
                out.push(c);
            }
            c if is_hidden(c) => out.push('\u{fffd}'),
            c => out.push(c),
        }
    }
}

/// The info strings a publisher-written fence may carry: a closed list, so `suggestion` (a
/// one-click change GitHub offers to apply) cannot be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Info {
    /// `text`: anything without a known language.
    Text,
    /// `rust`.
    Rust,
    /// `toml`.
    Toml,
    /// `json`.
    Json,
    /// `yaml`.
    Yaml,
    /// `sh`.
    Shell,
}

impl Info {
    /// Every info string, for tests of the allowlist.
    #[cfg(test)]
    const ALL: [Self; 6] = [
        Self::Text,
        Self::Rust,
        Self::Toml,
        Self::Json,
        Self::Yaml,
        Self::Shell,
    ];

    /// The info string for code from `path`, by its extension.
    pub(super) fn for_path(path: &str) -> Self {
        let extension = path
            .rsplit_once('/')
            .map_or(path, |(_, file)| file)
            .rsplit_once('.')
            .map(|(_, extension)| extension);
        match extension {
            Some("rs") => Self::Rust,
            Some("toml") => Self::Toml,
            Some("json") => Self::Json,
            Some("yml" | "yaml") => Self::Yaml,
            Some("sh") => Self::Shell,
            _ => Self::Text,
        }
    }

    /// The info string as written after the fence.
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Rust => "rust",
            Self::Toml => "toml",
            Self::Json => "json",
            Self::Yaml => "yaml",
            Self::Shell => "sh",
        }
    }
}

/// `code` in a fenced block tagged `info`: at most [`MAX_CODE_LINES`] lines and
/// [`MAX_CODE_CHARS`] characters (then a `…` line), control and format characters other than
/// tabs replaced by U+FFFD, under a backtick fence longer than any backtick run inside, so
/// nothing in it can close the block.
pub(super) fn code_block(code: &str, info: Info) -> String {
    let (clipped, mut cut) = clip(code, MAX_CODE_CHARS);
    let mut body = String::with_capacity(clipped.len());
    for (index, line) in clipped.split('\n').enumerate() {
        if index == MAX_CODE_LINES {
            cut = true;
            break;
        }
        if index > 0 {
            body.push('\n');
        }
        let line = line.strip_suffix('\r').unwrap_or(line);
        body.extend(line.chars().map(|c| {
            if c != '\t' && is_hidden(c) {
                '\u{fffd}'
            } else {
                c
            }
        }));
    }
    if cut {
        body.push('\n');
        body.push(ELLIPSIS);
    }
    let longest = body.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.saturating_add(1).max(3));
    [fence.as_str(), info.as_str(), "\n", &body, "\n", &fence].concat()
}

/// A repository `owner/name` as GitHub spells them: ASCII letters, digits and `-` for the
/// owner, plus `.` and `_` for the name, which is never `.` or `..`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Repository<'a> {
    owner: &'a str,
    name: &'a str,
}

impl<'a> Repository<'a> {
    /// `full_name` when it has that shape.
    pub(super) fn parse(full_name: &'a str) -> Option<Self> {
        let (owner, name) = full_name.split_once('/')?;
        let owner_ok = valid_name(owner, |byte| byte.is_ascii_alphanumeric() || byte == b'-');
        let name_ok = valid_name(name, |byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_')
        }) && name != "."
            && name != "..";
        (owner_ok && name_ok).then_some(Self { owner, name })
    }
}

/// Whether `name` is 1 to [`MAX_NAME_BYTES`] bytes, each `allowed`.
fn valid_name(name: &str, allowed: impl Fn(u8) -> bool) -> bool {
    (1..=MAX_NAME_BYTES).contains(&name.len()) && name.bytes().all(allowed)
}

/// A live link to line `line` of `path` at `commit` in `repository`, its text `path:line`
/// escaped as prose. A path that could leave the repository's tree once a browser resolves it
/// (empty, `.` or `..` segments, a leading `/`) or is over [`MAX_PATH_BYTES`] gets no link,
/// only the escaped text. Every path byte outside `A-Z a-z 0-9 - . _ ~ /` is percent-encoded,
/// so the destination holds no space, parenthesis or angle bracket.
pub(super) fn permalink(
    repository: Repository<'_>,
    commit: &CommitId,
    path: &str,
    line: NonZeroU64,
) -> String {
    // `:` and up to 20 digits follow the path.
    let text = prose(&format!("{path}:{line}"), MAX_PATH_BYTES.saturating_add(21));
    let stays_in_tree = path.len() <= MAX_PATH_BYTES
        && path
            .split('/')
            .all(|segment| !matches!(segment, "" | "." | ".."));
    if !stays_in_tree {
        return text;
    }
    let mut url = format!(
        "https://github.com/{}/{}/blob/{}/",
        repository.owner,
        repository.name,
        commit.as_str()
    );
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            url.push(char::from(byte));
        } else {
            url.push('%');
            url.extend([byte >> 4, byte & 0x0f].map(hex_digit));
        }
    }
    format!("[{text}]({url}#L{line})")
}

/// The uppercase hexadecimal digit of `nibble`'s low four bits.
const fn hex_digit(nibble: u8) -> char {
    match nibble & 0x0f {
        0 => '0',
        1 => '1',
        2 => '2',
        3 => '3',
        4 => '4',
        5 => '5',
        6 => '6',
        7 => '7',
        8 => '8',
        9 => '9',
        10 => 'A',
        11 => 'B',
        12 => 'C',
        13 => 'D',
        14 => 'E',
        _ => 'F',
    }
}

#[cfg(test)]
mod tests;
