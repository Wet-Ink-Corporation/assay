//! Sanitisers for PR-controlled text in a finding's title or evidence.
//!
//! Every producer of findings (the `assay-self` replay here, the temper adapter in
//! `assay-gates`) shares these, so they cannot drift apart (#189). Both are linear in their
//! input.

/// A PR-controlled name (a path, a lint, a crate) made safe for a title or a code span.
///
/// Anything but ASCII letters, digits and `_:-./` becomes `?`, so no backtick, newline,
/// control, bidi, zero-width or markup character gets through, and `://` becomes `?` so no URL
/// renders as a link where the name is not in a code span.
///
/// ```
/// assert_eq!(assay_app::plain_name("x`\n[l](https://e)"), "x???l??https?e?");
/// ```
#[must_use]
pub fn plain_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "_:-./".contains(c) {
                c
            } else {
                '?'
            }
        })
        .collect::<String>()
        .replace("://", "?")
}

/// `text` cut to its first `max` characters (not bytes, so no character is split).
///
/// ```
/// assert_eq!(assay_app::clip_chars("ééé", 2), "éé");
/// ```
#[must_use]
pub fn clip_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_characters_become_question_marks() {
        assert_eq!(
            plain_name("a\0b\tc\rd\ne\u{1b}f\u{7f}g\u{85}h"),
            "a?b?c?d?e?f?g?h",
            "NUL, TAB, CR, LF, ESC, DEL and NEL are all replaced"
        );
    }

    #[test]
    fn bidi_and_zero_width_characters_become_question_marks() {
        assert_eq!(
            plain_name("a\u{202e}b\u{2066}c\u{200b}d\u{200d}e\u{feff}f"),
            "a?b?c?d?e?f",
            "RLO, LRI, ZWSP, ZWJ and BOM cannot reorder or hide text"
        );
    }

    #[test]
    fn the_allowed_set_is_kept_and_a_scheme_separator_is_defanged() {
        assert_eq!(
            plain_name("crates/a_b-c.rs::x https://e"),
            "crates/a_b-c.rs::x?https?e",
            "letters, digits and `_:-./` stay; a space and `://` do not"
        );
    }

    #[test]
    fn clipping_keeps_text_at_the_cap_and_drops_one_past_it() {
        assert_eq!(
            (clip_chars("abcd", 4), clip_chars("abcde", 4)),
            ("abcd".to_owned(), "abcd".to_owned()),
            "cap characters are kept whole; cap + 1 loses the last one"
        );
    }

    #[test]
    fn a_multibyte_character_at_the_cap_is_kept_or_dropped_whole() {
        assert_eq!(
            (clip_chars("ab\u{1f600}", 3), clip_chars("ab\u{1f600}c", 2)),
            ("ab\u{1f600}".to_owned(), "ab".to_owned()),
            "a four-byte character is counted once and never split"
        );
    }
}
