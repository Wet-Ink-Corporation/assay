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
use super::tokens::{Source, Token};

/// A test failure.
type Failure = Box<dyn Error>;

/// What the checks must do with a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// Lex it token for token as proc-macro2 does, after rustc's normalisation.
    Agrees,
    /// Report a blind spot: it does not lex, or holds an attribute the scan does not read.
    Blind,
}

/// The forms review round 3 named, each with what the checks must do with it.
const CASES: [(&str, Expected); 26] = [
    (
        "m!(r\"x\"\r\n);\r\nconst S: &str = \"a\r\nb\";\r\n",
        Expected::Agrees,
    ),
    ("\u{FEFF}fn f() {}", Expected::Agrees),
    (
        "const C: char = '\u{2118}'; const B: u8 = b'\u{7F}';",
        Expected::Agrees,
    ),
    (
        "/* caf\u{E9} \u{200E} */ fn f() {} // \u{2028}\n",
        Expected::Agrees,
    ),
    ("fn\u{0B}f\u{0C}() {}\r", Expected::Agrees),
    ("const S: &str = \"\u{85}\u{24B6}\";", Expected::Agrees),
    (
        "#[cfg_attr\u{200E}(all(), path = \"x.rs\")]",
        Expected::Blind,
    ),
    ("#\u{200E}[deny(x)]", Expected::Blind),
    ("#!\u{200E}[deny(x)]", Expected::Blind),
    ("fn f() {}\u{2029}", Expected::Blind),
    ("fn f() {}\u{85}", Expected::Blind),
    ("fn f\u{200F}() {}", Expected::Blind),
    ("skip!(x\u{B7}r\"\\\"\");", Expected::Blind),
    ("m!('\u{2118} '\\\"');", Expected::Blind),
    ("fn \u{2118}() {}", Expected::Blind),
    ("fn \u{24B6}() {}", Expected::Blind),
    ("let x\u{345} = 1;", Expected::Blind),
    (
        "\u{FEFF}#!/bin/x r\"\n#[path = \"x.rs\"]\nmod m;",
        Expected::Blind,
    ),
    ("---\nx = \"\n---\nfn f() {}", Expected::Blind),
    ("#[deny $a]", Expected::Blind),
    ("#[expect $a]", Expected::Blind),
    ("#[cfg_attr $a]", Expected::Blind),
    ("$h [deny(x)]", Expected::Blind),
    ("include!(\"gen.txt\");", Expected::Blind),
    ("include!(concat!(\"a\", \".rs\"));", Expected::Blind),
    ("include_bytes!(env!(\"X\"));", Expected::Blind),
];

/// Whether the checks report a blind spot in `text`.
fn blind(text: &str) -> bool {
    Source::new(text)
        .tokens()
        .map_or(true, |lexed| !attributes::unread(&lexed).is_empty())
}

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

/// The tokens of `source` as written, for comparing with the reference.
fn ours(source: &Source<'_>) -> Result<Vec<String>, Failure> {
    let lexed = source
        .tokens()
        .map_err(|unlexable| format!("{unlexable:?}"))?;
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
    let source = Source::new(CORPUS);
    assert_eq!(
        ours(&source)?,
        reference(source.text())?,
        "lifetimes, prefixes, suffixes, numbers and raw names"
    );
    Ok(())
}

#[test]
fn each_review_case_agrees_with_proc_macro2_or_is_a_blind_spot() {
    let mut seen = Vec::with_capacity(CASES.len());
    for (text, _) in CASES {
        let source = Source::new(text);
        let agrees = ours(&source)
            .is_ok_and(|ours| reference(source.text()).is_ok_and(|reference| ours == reference));
        let outcome = match (agrees, blind(text)) {
            (true, false) => Some(Expected::Agrees),
            (_, true) => Some(Expected::Blind),
            (false, false) => None,
        };
        seen.push((text, outcome));
    }

    let wanted: Vec<_> = CASES
        .iter()
        .map(|(text, expected)| (*text, Some(*expected)))
        .collect();
    assert_eq!(seen, wanted, "no case lexes differently and passes");
}

#[test]
fn the_lexer_ends_tokens_where_proc_macro2_does_on_this_workspace() -> Result<(), Failure> {
    let sources = workspace_sources()?;
    assert!(sources.len() > 50, "the workspace was found: {sources:?}");
    for path in sources {
        let text = fs::read_to_string(&path)?;
        let source = Source::new(&text);
        assert_eq!(
            ours(&source)?,
            reference(source.text())?,
            "{}",
            path.display()
        );
    }
    Ok(())
}

#[test]
fn this_workspace_has_no_attribute_the_scan_cannot_read() -> Result<(), Failure> {
    let mut unread = Vec::new();
    for path in workspace_sources()? {
        let text = fs::read_to_string(&path)?;
        let source = Source::new(&text);
        let lexed = source
            .tokens()
            .map_err(|unlexable| format!("{}: {unlexable:?}", path.display()))?;
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
