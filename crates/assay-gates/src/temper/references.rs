//! The files a Rust source pulls in by name: `include!` targets and `mod name;` declarations,
//! which `head.rs` resolves against the head tree (03 §2.1 step 2). A file that does not exist
//! there can be written by a build script before rustc compiles it, after the scan ran.

use std::num::NonZeroU64;

use super::attributes::{INCLUDES, group};
use super::tokens::{Lexed, Token};

/// One file reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Reference<'a> {
    /// `include!` of a literal path, which rustc resolves from the including file's directory.
    Include(&'a str),
    /// `mod name;` at the top level of the file.
    Module(&'a str),
    /// A reference no file can be named for: an `include!` of anything but one string literal,
    /// an `include_str!` or `include_bytes!` of anything but one literal, an `include!` or
    /// `mod name;` in a `macro_rules!` body (rustc resolves it from the caller's file), a
    /// `mod name;` inside braces (an inline module or a block), and `mod $name;`.
    Unresolvable,
}

/// Every file reference in `tokens`, with the line it starts on, in order.
pub(super) fn references<'a>(tokens: &[Lexed<'a>]) -> Vec<(NonZeroU64, Reference<'a>)> {
    let mut found = Vec::new();
    let mut depth = 0_usize;
    // How many tokens of the `macro_rules!` body being read are still ahead.
    let mut macro_left = 0_usize;
    let mut rest = tokens;
    while let [lexed, tail @ ..] = rest {
        rest = tail;
        let in_macro = macro_left > 0;
        macro_left = macro_left.saturating_sub(1);
        let reference = match lexed.token {
            Token::Punct('{') => {
                depth = depth.saturating_add(1);
                None
            }
            Token::Punct('}') => {
                depth = depth.saturating_sub(1);
                None
            }
            Token::Ident {
                name: "macro_rules",
                ..
            } => {
                if let Some(length) = macro_body_length(tail) {
                    macro_left = macro_left.max(length);
                }
                None
            }
            Token::Ident { name, .. } if INCLUDES.contains(&name) => included(name, tail, in_macro),
            Token::Ident { name: "mod", .. } => module(tail, depth > 0 || in_macro),
            Token::Ident { .. } | Token::Lifetime(_) | Token::Literal(_) | Token::Punct(_) => None,
        };
        if let Some(reference) = reference {
            found.push((lexed.line, reference));
        }
    }
    found
}

/// The number of tokens from `!` to the end of a `macro_rules! name { .. }` body, when `tail`
/// (the tokens after `macro_rules`) is one.
fn macro_body_length(tail: &[Lexed<'_>]) -> Option<usize> {
    let [bang, name, open, inner @ ..] = tail else {
        return None;
    };
    let shaped = bang.token == Token::Punct('!')
        && matches!(name.token, Token::Ident { .. })
        && matches!(open.token, Token::Punct('(' | '[' | '{'));
    // `!`, the name, the opening bracket, the body and its closing bracket.
    shaped.then(|| group(inner).len().saturating_add(4))
}

/// The reference a call of `name` (one of the include macros) makes, from the tokens after its
/// name. A use of the name without `!` is `attributes::unread`'s.
fn included<'a>(name: &str, tail: &[Lexed<'a>], in_macro: bool) -> Option<Reference<'a>> {
    let [bang, after_bang @ ..] = tail else {
        return None;
    };
    if bang.token != Token::Punct('!') {
        return None;
    }
    let literal = only_literal(after_bang);
    if name != "include" {
        return literal.is_none().then_some(Reference::Unresolvable);
    }
    Some(match literal.and_then(string_text) {
        Some(path) if !in_macro => Reference::Include(path),
        Some(_) | None => Reference::Unresolvable,
    })
}

/// The literal a macro call's arguments hold when they are exactly one literal, with an
/// optional trailing comma.
fn only_literal<'a>(after_bang: &[Lexed<'a>]) -> Option<&'a str> {
    let [open, inner @ ..] = after_bang else {
        return None;
    };
    if !matches!(open.token, Token::Punct('(' | '[' | '{')) {
        return None;
    }
    match group(inner) {
        [only] => match only.token {
            Token::Literal(literal) => Some(literal),
            Token::Ident { .. } | Token::Lifetime(_) | Token::Punct(_) => None,
        },
        [only, comma] if comma.token == Token::Punct(',') => match only.token {
            Token::Literal(literal) => Some(literal),
            Token::Ident { .. } | Token::Lifetime(_) | Token::Punct(_) => None,
        },
        _ => None,
    }
}

/// The text of a string literal with no escapes and no suffix, plain (`"a"`) or raw (`r#"a"#`).
fn string_text(literal: &str) -> Option<&str> {
    let quoted = match literal.strip_prefix('r') {
        Some(raw) => {
            let hashes = raw.len().saturating_sub(raw.trim_start_matches('#').len());
            raw.get(hashes..)?.strip_suffix(raw.get(..hashes)?)?
        }
        None if literal.contains('\\') => return None,
        None => literal,
    };
    quoted.strip_prefix('"')?.strip_suffix('"')
}

/// The reference a `mod` makes, from the tokens after it: `mod name;` names a file, `mod name
/// { .. }` does not.
fn module<'a>(tail: &[Lexed<'a>], nested: bool) -> Option<Reference<'a>> {
    match tail {
        [name, semicolon, ..] if semicolon.token == Token::Punct(';') => match name.token {
            Token::Ident { name, .. } if !nested => Some(Reference::Module(name)),
            Token::Ident { .. } | Token::Lifetime(_) | Token::Literal(_) | Token::Punct(_) => {
                Some(Reference::Unresolvable)
            }
        },
        [dollar, ..] if dollar.token == Token::Punct('$') => Some(Reference::Unresolvable),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::tokens::Source;
    use super::*;

    /// The references in `text`, without lines, as their debug form.
    fn in_text(text: &str) -> Vec<String> {
        let source = Source::new(text);
        let lexed = source.tokens().expect("terminated");
        references(&lexed)
            .into_iter()
            .map(|(_, reference)| format!("{reference:?}"))
            .collect()
    }

    #[test]
    fn an_include_names_its_literal_path() {
        assert_eq!(
            in_text("include!(\"a.rs\"); include! { r##\"b/c.rs\"##, } std::include![\"d\"];"),
            ["Include(\"a.rs\")", "Include(\"b/c.rs\")", "Include(\"d\")"],
            "plain and raw strings, any brackets"
        );
    }

    #[test]
    fn an_include_with_no_literal_path_is_unresolvable() {
        for text in [
            "include!(concat!(\"a\", \".rs\"));",
            "include!(\"a\\x2ers\");",
            "include!(\"a.rs\"x);",
            "include!(b\"a.rs\");",
            "include!(\"a.rs\", \"b.rs\");",
            "include!;",
            "include_str!(env!(\"X\"));",
            "macro_rules! m { () => { include!(\"a.rs\"); }; }",
        ] {
            assert_eq!(in_text(text), ["Unresolvable"], "{text}");
        }
    }

    #[test]
    fn data_includes_of_a_literal_name_nothing() {
        assert_eq!(
            in_text("include_str!(\"a.json\"); include_bytes!(\"b.bin\"); x!(include);"),
            Vec::<String>::new(),
            "they embed bytes, and the scan reads every `.rs` file anyway"
        );
    }

    #[test]
    fn a_module_declaration_names_its_file_only_at_the_top_level() {
        assert_eq!(
            in_text(
                "pub(crate) mod a; mod b { mod c; } fn f() { mod d; } \
                 macro_rules! m { ($n:ident) => { mod $n; mod e; }; } mod g;"
            ),
            [
                "Module(\"a\")",
                "Unresolvable",
                "Unresolvable",
                "Unresolvable",
                "Unresolvable",
                "Module(\"g\")"
            ],
            "nested and macro-made declarations resolve from elsewhere"
        );
    }
}
