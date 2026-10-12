//! What one Rust file reads at compile time, from #63's tokens (`docs/architecture/04-pipeline.md`
//! §2.2 and §2.3): the literal paths of its include macros and `#[path]` attributes, and its
//! top-level `mod name;` declarations. `closure.rs` resolves them.
//!
//! Nothing is expanded (Weigh-In default wi-b5affa). Any form whose target this scan cannot
//! be sure of makes the file `uncertain`: an include macro called with anything but one plain
//! string literal, inside a `macro_rules!` body (its path resolves from the caller) or not
//! called with `!` (a rename); a `#[path]` that is not `#[path = "literal"]` on a top-level
//! `mod name;`; a `mod` inside any bracket or a macro body, or named by a fragment; a
//! `macro_rules!` below the top level; a `cfg_attr` naming `path` or a fragment; a raw-named
//! attribute; a `#` that does not start an attribute; and a fragment that can assemble one.

use super::attributes::{INCLUDES, Unread, group, unread_why};
use super::tokens::{Lexed, Token};

/// What an include macro reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Include {
    /// `include!`: Rust, parsed in the including file.
    Code,
    /// `include_str!` or `include_bytes!`: data.
    Data,
}

/// What one file reads at compile time, its paths as written.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct FileReach {
    /// Each include macro call's literal path.
    pub(super) includes: Vec<(Include, String)>,
    /// Each top-level `mod name;`, with its `#[path = ..]` literal when it has one.
    pub(super) modules: Vec<(String, Option<String>)>,
    /// Whether the file holds a form this scan cannot place (see the module documentation).
    pub(super) uncertain: bool,
}

/// The reach of a file whose tokens are `tokens`.
pub(super) fn scan(tokens: &[Lexed<'_>]) -> FileReach {
    let mut found = FileReach {
        uncertain: unread_why(tokens).iter().any(|(_, why)| {
            matches!(
                why,
                Unread::CfgAttrPath
                    | Unread::RenamedInclude
                    | Unread::Raw
                    | Unread::Fragment
                    | Unread::Assembled
            )
        }),
        ..FileReach::default()
    };
    let mut depth = 0_usize;
    // How many tokens of the `macro_rules!` body being read are still ahead.
    let mut macro_left = 0_usize;
    // The index of a `mod` a `#[path]` already recorded.
    let mut recorded_mod = None;
    for (index, lexed) in tokens.iter().enumerate() {
        let tail = tokens.get(index.saturating_add(1)..).unwrap_or(&[]);
        let in_macro = macro_left > 0;
        let nested = depth > 0 || in_macro;
        macro_left = macro_left.saturating_sub(1);
        match lexed.token {
            Token::Punct('(' | '[' | '{') => depth = depth.saturating_add(1),
            Token::Punct(')' | ']' | '}') => depth = depth.saturating_sub(1),
            Token::Ident {
                name: "macro_rules",
                ..
            } => {
                macro_left = macro_left.max(macro_body_length(tail));
                found.uncertain |= nested;
            }
            Token::Ident { name, .. } if INCLUDES.contains(&name) && opens(tail, '!') => {
                record_include(&mut found, name, tail, in_macro);
            }
            Token::Ident { name: "mod", .. } if recorded_mod != Some(index) => {
                module(&mut found, tail, nested);
            }
            Token::Punct('#') => match path_attribute(tail) {
                PathAttribute::None => {}
                PathAttribute::Module {
                    path,
                    name,
                    mod_offset,
                } if !nested => {
                    found.modules.push((name, Some(path)));
                    recorded_mod = Some(index.saturating_add(1).saturating_add(mod_offset));
                }
                PathAttribute::Module { .. } | PathAttribute::Unplaced => found.uncertain = true,
            },
            _ => {}
        }
    }
    found
}

/// Records a call of the include macro `name`, from the tokens after its name. In a
/// `macro_rules!` body its path resolves from the caller, so it is uncertain there.
fn record_include(found: &mut FileReach, name: &str, tail: &[Lexed<'_>], in_macro: bool) {
    let kind = if name == "include" {
        Include::Code
    } else {
        Include::Data
    };
    match tail.get(1..).and_then(one_literal) {
        Some(path) if !in_macro => found.includes.push((kind, path)),
        _ => found.uncertain = true,
    }
}

/// Records a `mod`, from the tokens after it: `mod name;` at the top level, and anything a
/// fragment names or a nested declaration as uncertain. `mod name { .. }` names no file.
fn module(found: &mut FileReach, tail: &[Lexed<'_>], nested: bool) {
    match tail {
        [name, semicolon, ..] if semicolon.token == Token::Punct(';') => match name.token {
            Token::Ident { name, .. } if !nested => found.modules.push((name.to_owned(), None)),
            _ => found.uncertain = true,
        },
        [dollar, ..] if dollar.token == Token::Punct('$') => found.uncertain = true,
        _ => {}
    }
}

/// Whether `tokens` starts with the punctuation `punct`.
fn opens(tokens: &[Lexed<'_>], punct: char) -> bool {
    tokens
        .first()
        .is_some_and(|lexed| lexed.token == Token::Punct(punct))
}

/// The number of tokens from `!` to the end of a `macro_rules! name { .. }` body, when `tail`
/// (the tokens after `macro_rules`) is one; 0 otherwise.
fn macro_body_length(tail: &[Lexed<'_>]) -> usize {
    let [bang, name, open, inner @ ..] = tail else {
        return 0;
    };
    let shaped = bang.token == Token::Punct('!')
        && matches!(name.token, Token::Ident { .. })
        && matches!(open.token, Token::Punct('(' | '[' | '{'));
    // `!`, the name, the opening bracket, the body and its closing bracket.
    if shaped {
        group(inner).len().saturating_add(4)
    } else {
        0
    }
}

/// The path a macro call's arguments name when they are exactly one plain string literal, with
/// an optional trailing comma.
fn one_literal(after_bang: &[Lexed<'_>]) -> Option<String> {
    let [open, inner @ ..] = after_bang else {
        return None;
    };
    if !matches!(open.token, Token::Punct('(' | '[' | '{')) {
        return None;
    }
    match group(inner) {
        [only] => literal_path(only),
        [only, comma] if comma.token == Token::Punct(',') => literal_path(only),
        _ => None,
    }
}

/// The text of a plain string literal (`"x"`, `r"x"`, `r#"x"#`) with no escape and no suffix.
fn literal_path(lexed: &Lexed<'_>) -> Option<String> {
    let Token::Literal(text) = lexed.token else {
        return None;
    };
    if let Some(quoted) = text.strip_prefix('"') {
        let inner = quoted.strip_suffix('"')?;
        return (!inner.contains(['\\', '"'])).then(|| inner.to_owned());
    }
    let raw = text.strip_prefix('r')?;
    let hashes = raw.len().saturating_sub(raw.trim_start_matches('#').len());
    let fence = "#".repeat(hashes);
    let inner = raw
        .strip_prefix(fence.as_str())?
        .strip_prefix('"')?
        .strip_suffix(fence.as_str())?
        .strip_suffix('"')?;
    Some(inner.to_owned())
}

/// What a `#` starts, as far as `#[path]` goes.
#[derive(Debug, PartialEq, Eq)]
enum PathAttribute {
    /// Not a `path` attribute.
    None,
    /// `#[path = "literal"]` on `mod name;`, past other outer attributes and a visibility;
    /// `mod_offset` is the `mod`'s index in the tokens after the `#`.
    Module {
        path: String,
        name: String,
        mod_offset: usize,
    },
    /// Any other `path` attribute.
    Unplaced,
}

/// The `#[path]` the tokens after a `#` start, if they start one.
fn path_attribute(after_hash: &[Lexed<'_>]) -> PathAttribute {
    let is_path = |lexed: &Lexed<'_>| matches!(lexed.token, Token::Ident { name: "path", .. });
    match after_hash {
        [bang, open, name, ..]
            if bang.token == Token::Punct('!')
                && open.token == Token::Punct('[')
                && is_path(name) =>
        {
            return PathAttribute::Unplaced;
        }
        [open, name, ..] if open.token == Token::Punct('[') && is_path(name) => {}
        _ => return PathAttribute::None,
    }
    let shaped = match after_hash {
        [_, _, equals, literal, close, rest @ ..]
            if equals.token == Token::Punct('=') && close.token == Token::Punct(']') =>
        {
            literal_path(literal).map(|path| (path, rest))
        }
        _ => None,
    };
    let Some((path, rest)) = shaped else {
        return PathAttribute::Unplaced;
    };
    let item = past_attributes_and_visibility(rest);
    let skipped = after_hash.len().saturating_sub(item.len());
    match item {
        [keyword, name, semicolon, ..]
            if matches!(keyword.token, Token::Ident { name: "mod", .. })
                && semicolon.token == Token::Punct(';') =>
        {
            match name.token {
                Token::Ident { name, .. } => PathAttribute::Module {
                    path,
                    name: name.to_owned(),
                    mod_offset: skipped,
                },
                _ => PathAttribute::Unplaced,
            }
        }
        _ => PathAttribute::Unplaced,
    }
}

/// `tokens` past any outer attributes (`#[..]`) and a visibility (`pub`, `pub(..)`).
fn past_attributes_and_visibility<'t, 'a>(tokens: &'t [Lexed<'a>]) -> &'t [Lexed<'a>] {
    let mut rest = tokens;
    while let [hash, open, inner @ ..] = rest {
        if hash.token != Token::Punct('#') || open.token != Token::Punct('[') {
            break;
        }
        rest = inner
            .get(group(inner).len().saturating_add(1)..)
            .unwrap_or(&[]);
    }
    match rest {
        [public, open, inner @ ..]
            if matches!(public.token, Token::Ident { name: "pub", .. })
                && open.token == Token::Punct('(') =>
        {
            inner
                .get(group(inner).len().saturating_add(1)..)
                .unwrap_or(&[])
        }
        [public, after @ ..] if matches!(public.token, Token::Ident { name: "pub", .. }) => after,
        rest => rest,
    }
}

#[cfg(test)]
mod tests {
    use super::super::tokens::Source;
    use super::*;

    fn of(text: &str) -> FileReach {
        let source = Source::new(text);
        scan(&source.tokens().expect("terminated"))
    }

    #[test]
    fn reads_literal_includes_and_path_modules() {
        assert_eq!(
            of(
                "include!(\"a.rs\"); fn f() { include_str!(r#\"b.md\"#,); }\n\
                #[path = \"c.rs\"] #[cfg(unix)] pub(crate) mod c;\nmod d;"
            ),
            FileReach {
                includes: vec![
                    (Include::Code, "a.rs".to_owned()),
                    (Include::Data, "b.md".to_owned())
                ],
                modules: vec![
                    ("c".to_owned(), Some("c.rs".to_owned())),
                    ("d".to_owned(), None)
                ],
                uncertain: false,
            },
            "the `mod` after a `#[path]` is recorded once"
        );
    }

    #[test]
    fn a_form_it_cannot_place_is_uncertain() {
        for text in [
            "include!(\"a\\x2ers\");",
            "include!(\"a.rs\"x);",
            "#[path(\"a.rs\")] mod a;",
            "#![path = \"a.rs\"]",
            "#[path = \"a.rs\"] mod a { }",
            "mod a { mod b; }",
            "m! { mod $n; }",
            "fn f() { macro_rules! m { () => {}; } }",
        ] {
            assert!(of(text).uncertain, "{text}");
        }
    }
}
