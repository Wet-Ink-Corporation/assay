//! The one shared normaliser of evidence (`docs/architecture/03-contracts.md` §3), used by the
//! fingerprint and by the evidence check (04 §4 step 2).

/// The wildcard that `...` and `…` both become.
const WILDCARD: &str = "\u{2026}";

/// The ASCII spelling of the wildcard.
const ASCII_WILDCARD: &str = "...";

/// Every quote character other than `"`; each becomes `"`.
const OTHER_QUOTES: [char; 6] = ['\'', '`', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}'];

/// Normalises `text` so that two quotations of the same code compare equal however they are
/// spaced, quoted or commented (03 §3).
///
/// - A trailing comment is dropped from each line: `//` (not after `:`, so URLs survive) or `#`
///   between whitespace, outside a `"` string. A line that is only a comment is kept whole.
/// - Whitespace runs, line breaks included, become one space; the ends are trimmed.
/// - `'`, `` ` ``, `‘`, `’`, `“` and `”` become `"`.
/// - `...` and `…` become the wildcard `…`, which absorbs the spaces beside it.
///
/// Linear in the length of `text`.
///
/// # Examples
///
/// ```
/// use assay_domain::normalise;
///
/// assert_eq!(normalise("let s =  'a' ; // why"), r#"let s = "a" ;"#);
/// assert_eq!(normalise("call( ... )"), "call(…)");
/// ```
#[must_use]
pub fn normalise(text: &str) -> String {
    let mut spaced = String::with_capacity(text.len());
    for word in text.lines().map(code_of).flat_map(str::split_whitespace) {
        if !spaced.is_empty() {
            spaced.push(' ');
        }
        spaced.extend(word.chars().map(unify_quote));
    }
    spaced
        .replace(ASCII_WILDCARD, WILDCARD)
        .split(WILDCARD)
        .map(|piece| piece.trim_matches(' '))
        .collect::<Vec<_>>()
        .join(WILDCARD)
}

/// `line` without its trailing comment; a line that is only a comment is returned whole.
fn code_of(line: &str) -> &str {
    let mut in_string = false;
    let mut escaped = false;
    let mut previous = None;
    for (index, c) in line.char_indices() {
        if in_string {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
        } else if c == '"' {
            in_string = true;
        } else if let Some((code, rest)) = line.split_at_checked(index)
            && starts_comment(previous, rest)
        {
            return if code.trim().is_empty() { line } else { code };
        }
        previous = Some(c);
    }
    line
}

/// Whether `rest`, which follows the character `previous`, opens a comment.
fn starts_comment(previous: Option<char>, rest: &str) -> bool {
    let slashes = rest.starts_with("//") && previous != Some(':');
    let hash = previous.is_none_or(char::is_whitespace)
        && rest
            .strip_prefix('#')
            .is_some_and(|after| after.is_empty() || after.starts_with(char::is_whitespace));
    slashes || hash
}

/// `c`, with every quote style made `"`.
fn unify_quote(c: char) -> char {
    if OTHER_QUOTES.contains(&c) { '"' } else { c }
}

#[cfg(test)]
mod tests {
    use super::normalise;

    #[test]
    fn collapses_whitespace_runs_and_trims_the_ends() {
        assert_eq!(normalise("\t let  x =\t\ty ;  "), "let x = y ;");
    }

    #[test]
    fn joins_lines_with_one_space() {
        assert_eq!(normalise("foo(\n    bar,\n)"), "foo( bar, )");
    }

    #[test]
    fn unifies_every_quote_style_to_a_double_quote() {
        assert_eq!(
            normalise("'a' `b` \u{2018}c\u{2019} \u{201c}d\u{201d} \"e\""),
            "\"a\" \"b\" \"c\" \"d\" \"e\""
        );
    }

    #[test]
    fn strips_a_trailing_line_comment() {
        assert_eq!(
            normalise("let x = y.unwrap(); // fine here"),
            "let x = y.unwrap();"
        );
    }

    #[test]
    fn strips_a_trailing_comment_on_every_line() {
        assert_eq!(normalise("a(); // one\nb(); // two"), "a(); b();");
    }

    #[test]
    fn keeps_slashes_inside_a_string() {
        assert_eq!(
            normalise(r#"let url = "a // b\" // c"; // d"#),
            r#"let url = "a // b\" // c";"#
        );
    }

    #[test]
    fn keeps_a_whole_line_comment_and_the_urls_in_it() {
        assert_eq!(
            normalise("/// See https://example.com/x for why."),
            "/// See https://example.com/x for why."
        );
    }

    #[test]
    fn keeps_a_url_after_code() {
        assert_eq!(normalise("see http://a/b"), "see http://a/b");
    }

    #[test]
    fn strips_a_trailing_hash_comment() {
        assert_eq!(normalise("key = 1 # why"), "key = 1");
    }

    #[test]
    fn keeps_hashes_that_are_not_comments() {
        assert_eq!(
            normalise("#[derive(Debug)] struct A; $( #[$m] )* r#\"x\"#;"),
            "#[derive(Debug)] struct A; $( #[$m] )* r#\"x\"#;"
        );
    }

    #[test]
    fn makes_both_ellipses_one_wildcard_that_absorbs_adjacent_spaces() {
        assert_eq!(
            normalise("foo( ... ) \u{2026} bar(\u{2026})"),
            "foo(\u{2026})\u{2026}bar(\u{2026})"
        );
    }

    #[test]
    fn normalises_blank_text_to_nothing() {
        assert_eq!(normalise(" \n\t "), "");
    }
}
