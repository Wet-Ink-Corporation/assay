//! The files a Rust source pulls in by name: `mod name;` declarations, which `head.rs`
//! resolves against the head tree (03 §2.1 step 2), and the include macros, which it refuses.
//! A file that does not exist there can be written by a build script before rustc compiles it,
//! after the scan ran.
//!
//! Where the reference cannot be modelled exactly it is refused rather than modelled
//! (Weigh-In default wi-b5affa): every `include!`, every `mod name;` that is not at the top
//! level of the file, and every `macro_rules` that is not, whose transcriber can be built from
//! another macro's fragments.

use std::num::NonZeroU64;

use super::attributes::{INCLUDES, group};
use super::tokens::{Lexed, Token};

/// One file reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Reference<'a> {
    /// `mod name;` at the top level of the file.
    Module(&'a str),
    /// A reference this scan does not follow: every `include!` (its text is parsed in the
    /// including file's context, which the scan does not model), an `include_str!` or
    /// `include_bytes!` of anything but one literal, a `mod name;` inside any bracket (an inline
    /// module, a block or a macro call's arguments) or in a `macro_rules!` body (rustc resolves
    /// it from the caller's file), and `mod $name;`.
    Unresolvable,
    /// A `macro_rules` (raw or not) inside any bracket or another `macro_rules!` body: an
    /// outer macro can bind a fragment of it to `$`, so its transcriber can assemble an
    /// attribute or a reference from call-site tokens this scan cannot see.
    NestedMacro,
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
            Token::Punct('(' | '[' | '{') => {
                depth = depth.saturating_add(1);
                None
            }
            Token::Punct(')' | ']' | '}') => {
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
                (depth > 0 || in_macro).then_some(Reference::NestedMacro)
            }
            Token::Ident { name, .. } if INCLUDES.contains(&name) => included(name, tail),
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
/// name: every `include!`, and a data include of anything but one literal. A use of the name
/// without `!` is `attributes::unread`'s.
fn included<'a>(name: &str, tail: &[Lexed<'a>]) -> Option<Reference<'a>> {
    let [bang, after_bang @ ..] = tail else {
        return None;
    };
    if bang.token != Token::Punct('!') {
        return None;
    }
    (name == "include" || !is_one_literal(after_bang)).then_some(Reference::Unresolvable)
}

/// Whether a macro call's arguments are exactly one literal, with an optional trailing comma.
fn is_one_literal(after_bang: &[Lexed<'_>]) -> bool {
    let [open, inner @ ..] = after_bang else {
        return false;
    };
    if !matches!(open.token, Token::Punct('(' | '[' | '{')) {
        return false;
    }
    match group(inner) {
        [only] => matches!(only.token, Token::Literal(_)),
        [only, comma] => {
            matches!(only.token, Token::Literal(_)) && comma.token == Token::Punct(',')
        }
        _ => false,
    }
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
    fn an_include_of_a_literal_path_is_unresolvable_too() {
        assert_eq!(
            in_text("include!(\"a.rs\"); include! { r##\"b/c.rs\"##, } std::include![\"d\"];"),
            ["Unresolvable", "Unresolvable", "Unresolvable"],
            "the included text is parsed in a context the scan does not model"
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
    fn a_macro_definition_is_read_only_at_the_top_level() {
        assert_eq!(
            in_text(
                "macro_rules! a { () => { macro_rules! b {} }; } fn f() { macro_rules! c {} } \
                 m!(r#macro_rules! d {}); [macro_rules] macro_rules! e { () => {}; }"
            ),
            ["NestedMacro", "NestedMacro", "NestedMacro", "NestedMacro"],
            "inside another body or any bracket, an outer macro can supply its `$`"
        );
    }

    #[test]
    fn a_module_declaration_names_its_file_only_at_the_top_level() {
        assert_eq!(
            in_text(
                "pub(crate) mod a; mod b { mod c; } fn f() { mod d; } \
                 macro_rules! m { ($n:ident) => { mod $n; mod e; }; } m!(mod h;) \
                 #[cfg(x)] [mod i;] mod g;"
            ),
            [
                "Module(\"a\")",
                "Unresolvable",
                "Unresolvable",
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
