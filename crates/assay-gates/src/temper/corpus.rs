//! The lexer against a reference: `proc-macro2`'s lexer (through `syn`'s token cursor), on a
//! corpus of the forms where lexers disagree and on every Rust file of this workspace; and the
//! workspace's own Rust against the attribute checks, since assay gates itself.

use std::error::Error;
use std::fs;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use syn::buffer::Cursor;
use syn::parse::{ParseStream, Parser};

use super::attributes;
use super::tokens::{Token, tokens};

/// A test failure.
type Failure = Box<dyn Error>;

/// Forms where a lexer can end a token somewhere other than rustc does.
const CORPUS: &str = r###"
fn f<'a, 'r, 'br>(x: &'a str) -> &'a str { 'outer: loop { break 'outer; } x }
const C: [char; 6] = ['a', '\'', '"', '\\', '\u{1F600}', '/'];
const B: [u8; 2] = [b'a', b'\''];
const S: [&str; 4] = ["a\"b", r"c\", r#"d"e"#, r##"f"#g"##];
const BS: [&[u8]; 2] = [b"x\"", br#"y"#];
const CS: [&core::ffi::CStr; 2] = [c"z", cr#"w"#];
const N: [f64; 6] = [1.5, 1e3, 1.5e-3, 0.1_f64, 2., 1E+2];
const I: [u64; 5] = [0x1F, 0o17, 0b1010_1010, 1_000, 7u64];
fn g() { let r#match = 1; let a = r#match; let b = 1..2; let c = (1, (2, 3)).1.0; let d = 1.max(2); }
m!('r "\"" "x"r 'a'r b'a'r r#r "y" 'br);
/* block /* nested */ still */
#[cfg_attr(all(), doc = "x")]
#![doc(hidden)]
"###;

/// The tokens of `text` as written, for comparing with the reference.
fn ours(text: &str) -> Result<Vec<String>, Failure> {
    let lexed = tokens(text).map_err(|unterminated| format!("{unterminated:?}"))?;
    Ok(lexed
        .into_iter()
        .map(|lexed| match lexed.token {
            Token::Ident { name, raw: false } => name.to_owned(),
            Token::Ident { name, raw: true } => format!("r#{name}"),
            Token::Lifetime(name) => format!("'{name}"),
            Token::Punct(c) => c.to_string(),
            Token::Literal(text) => text.to_owned(),
        })
        .collect())
}

/// `text` lexed by proc-macro2's fallback lexer, groups flattened into their brackets, without
/// the `#[doc = ..]` attributes it makes of doc comments (the lexer drops every comment).
fn reference(text: &str) -> Result<Vec<String>, Failure> {
    let parser = |input: ParseStream<'_>| {
        input.step(|cursor| {
            let mut out = Vec::new();
            let rest = flatten(*cursor, &mut out);
            Ok((out, rest))
        })
    };
    Ok(without_docs(&parser.parse_str(text)?))
}

fn flatten<'c>(mut cursor: Cursor<'c>, out: &mut Vec<String>) -> Cursor<'c> {
    loop {
        if let Some((lifetime, next)) = cursor.lifetime() {
            out.push(format!("'{}", lifetime.ident));
            cursor = next;
        } else if let Some((ident, next)) = cursor.ident() {
            out.push(ident.to_string());
            cursor = next;
        } else if let Some((punct, next)) = cursor.punct() {
            out.push(punct.as_char().to_string());
            cursor = next;
        } else if let Some((literal, next)) = cursor.literal() {
            out.push(literal.to_string());
            cursor = next;
        } else if let Some((inside, delimiter, _, next)) = cursor.any_group() {
            let (open, close) = match format!("{delimiter:?}").as_str() {
                "Parenthesis" => ("(", ")"),
                "Brace" => ("{", "}"),
                "Bracket" => ("[", "]"),
                _ => ("", ""),
            };
            out.push(open.to_owned());
            flatten(inside, out);
            out.push(close.to_owned());
            cursor = next;
        } else {
            return cursor;
        }
    }
}

/// `tokens` without `#[doc = ..]` and `#![doc = ..]`.
fn without_docs(tokens: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(tokens.len());
    let mut rest = tokens;
    while let [first, tail @ ..] = rest {
        let after_bang = match tail {
            [bang, after @ ..] if bang == "!" => after,
            after => after,
        };
        if first == "#"
            && let [open, doc, equals, _, close, after @ ..] = after_bang
            && [open, doc, equals, close].map(String::as_str) == ["[", "doc", "=", "]"]
        {
            rest = after;
            continue;
        }
        out.push(first.clone());
        rest = tail;
    }
    out
}

/// Every `.rs` file of the workspace's crates, outside `target`, in path order.
fn workspace_sources() -> Result<Vec<PathBuf>, Failure> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut found = Vec::new();
    let mut pending = vec![root.join("crates")];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let path = entry?.path();
            if path.is_dir() && !path.ends_with("target") {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                found.push(path);
            }
        }
    }
    found.sort();
    Ok(found)
}

#[test]
fn the_lexer_ends_tokens_where_proc_macro2_does_on_the_corpus() -> Result<(), Failure> {
    assert_eq!(
        ours(CORPUS)?,
        reference(CORPUS)?,
        "lifetimes, prefixes, suffixes, numbers and raw names"
    );
    Ok(())
}

#[test]
fn the_lexer_ends_tokens_where_proc_macro2_does_on_this_workspace() -> Result<(), Failure> {
    let sources = workspace_sources()?;
    assert!(sources.len() > 50, "the workspace was found: {sources:?}");
    for path in sources {
        let text = fs::read_to_string(&path)?;
        assert_eq!(ours(&text)?, reference(&text)?, "{}", path.display());
    }
    Ok(())
}

#[test]
fn this_workspace_has_no_attribute_the_scan_cannot_read() -> Result<(), Failure> {
    let mut unread = Vec::new();
    for path in workspace_sources()? {
        let text = fs::read_to_string(&path)?;
        let lexed = tokens(&text).map_err(|unterminated| format!("{unterminated:?}"))?;
        let lines: Vec<u64> = attributes::unread(&lexed)
            .into_iter()
            .map(NonZeroU64::get)
            .collect();
        if !lines.is_empty() {
            unread.push((path, lines));
        }
    }

    assert_eq!(
        unread,
        Vec::new(),
        "assay's own temper gate would fail on these"
    );
    Ok(())
}
