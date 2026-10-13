//! Fixtures for each construct the sanitiser neutralises, its caps and its invariants.

use std::num::NonZeroU64;

use proptest::prelude::*;

use super::{
    Info, MAX_CODE_CHARS, MAX_CODE_LINES, MAX_PATH_BYTES, Repository, code_block, permalink, prose,
};
use crate::config::CommitId;

/// Room for every fixture: none is clipped.
const ROOMY: usize = 10_000;

fn inert(text: &str) -> String {
    prose(text, ROOMY)
}

/// Each construct model prose might use, and how it is published (one fixture each).
const FIXTURES: [(&str, &str, &str); 36] = [
    (
        "inline link",
        "see [docs](https://evil.example/x)",
        r"see \[docs\]\(https\[\:\]\/\/evil\.example\/x\)",
    ),
    (
        "image",
        "![x](https://evil/p.png)",
        r"\!\[x\]\(https\[\:\]\/\/evil\/p\.png\)",
    ),
    (
        "angle autolink",
        "<https://evil>",
        r"\<https\[\:\]\/\/evil\>",
    ),
    (
        "bare URL",
        "go to http://evil.example now",
        r"go to http\[\:\]\/\/evil\.example now",
    ),
    (
        "www autolink in any case",
        "visit WWW.evil.com",
        r"visit WWW\[\.\]evil\.com",
    ),
    (
        "raw HTML",
        "<img src=x onerror=alert(1)>",
        r"\<img src\=x onerror\=alert\(1\)\>",
    ),
    ("HTML comment", "<!-- hidden -->", r"\<\!\-\- hidden \-\-\>"),
    (
        "details block",
        "<details><summary>x</summary>",
        r"\<details\>\<summary\>x\<\/summary\>",
    ),
    ("mention", "cc @owner please", "cc \u{ff20}owner please"),
    ("team mention", "@org/team", "\u{ff20}org\\/team"),
    ("email autolink", "a@b.com", "a\u{ff20}b\\.com"),
    ("numeric entity", "&#64;owner", "\\&\u{ff03}64\\;owner"),
    ("named entity", "&lt;b&gt;", r"\&lt\;b\&gt\;"),
    ("ATX heading", "# Approve", "\u{ff03} Approve"),
    (
        "cross-repository reference",
        "see other/repo#12",
        "see other\\/repo\u{ff03}12",
    ),
    ("emphasis", "**bold** _it_", r"\*\*bold\*\* \_it\_"),
    ("strikethrough", "~~gone~~", r"\~\~gone\~\~"),
    ("table row", "| a | b |", r"\| a \| b \|"),
    ("code span", "`x`", r"\`x\`"),
    (
        "suggestion fence",
        "```suggestion\nfoo\n```",
        "\\`\\`\\`suggestion\nfoo\n\\`\\`\\`",
    ),
    (
        "tilde fence",
        "~~~suggestion\nfoo\n~~~",
        "\\~\\~\\~suggestion\nfoo\n\\~\\~\\~",
    ),
    ("block quote", "> quoted", r"\> quoted"),
    (
        "list markers",
        "- item\n1. one\n2) two",
        "\\- item\n1\\. one\n2\\) two",
    ),
    (
        "reference definition",
        "[x]: https://evil",
        r"\[x\]\: https\[\:\]\/\/evil",
    ),
    ("pre-escaped bracket", r"\[x](y)", r"\\\[x\]\(y\)"),
    ("indented code", "    code", "code"),
    ("setext underline", "Title\n===", "Title\n\\=\\=\\="),
    ("thematic break", "***", r"\*\*\*"),
    ("hard break backslash", "a\\\nb", "a\\\\\nb"),
    (
        "bidi and zero-width",
        "a\u{202e}b\u{200d}c",
        "a\u{fffd}b\u{fffd}c",
    ),
    ("lone carriage return", "a\rb", "a\u{fffd}b"),
    ("CRLF line ending", "a\r\nb", "a\nb"),
    ("blank lines", "\n a\n\n\n\nb\n", "a\n\nb"),
    ("letters and symbols", "é 日本 ✓ —", "é 日本 ✓ —"),
    (
        "GH- issue reference in any case",
        "see GH-26 and gh-1",
        r"see GH\[\-\]26 and gh\[\-\]1",
    ),
    ("gh- without a digit", "gh-pages", r"gh\-pages"),
];

#[test]
fn each_construct_is_published_inert() {
    for (construct, model, published) in FIXTURES {
        assert_eq!(inert(model), published, "{construct}");
    }
}

#[test]
fn prose_of_the_cap_is_kept_whole() {
    assert_eq!(prose(&"a".repeat(5), 5), "aaaaa");
}

#[test]
fn prose_past_the_cap_is_clipped_and_marked() {
    assert_eq!(prose(&"é".repeat(6), 5), "ééééé…");
}

#[test]
fn clipping_counts_characters_before_escaping() {
    assert_eq!(prose("<<<", 2), r"\<\<…");
}

/// Undoes the backslash escapes, failing on any ASCII punctuation left unescaped or any
/// escape of something else: the published text must parse as plain text only.
fn rendered(published: &str) -> Result<String, String> {
    let mut text = String::new();
    let mut chars = published.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(escaped) if escaped.is_ascii_punctuation() => text.push(escaped),
                other => return Err(format!("a bare backslash before {other:?}")),
            },
            c if c.is_ascii_punctuation() => return Err(format!("unescaped {c:?}")),
            c => text.push(c),
        }
    }
    Ok(text)
}

/// Text built from the pieces most likely to form a construct.
fn hostile() -> impl Strategy<Value = String> {
    let piece = prop_oneof![
        Just("www.".to_owned()),
        Just("WwW.".to_owned()),
        Just("GH-".to_owned()),
        Just("gH-".to_owned()),
        "[0-9]",
        Just("://".to_owned()),
        Just(":/".to_owned()),
        Just("/".to_owned()),
        Just("@".to_owned()),
        Just("#".to_owned()),
        Just("\\".to_owned()),
        Just("\n".to_owned()),
        Just("\r".to_owned()),
        Just("    ".to_owned()),
        Just("\u{200d}".to_owned()),
        "[<>!\\[\\]`*_|~&]",
        "[a-z]{1,3}",
        any::<char>().prop_map(String::from),
    ];
    prop::collection::vec(piece, 0..40).prop_map(|pieces| pieces.concat())
}

proptest! {
    #[test]
    fn prose_always_renders_as_plain_text(model in hostile()) {
        let published = inert(&model);

        let text = rendered(&published).map_err(TestCaseError::fail)?;
        prop_assert!(!text.contains('@'), "{text:?}");
        prop_assert!(!text.contains('#'), "{text:?}");
        prop_assert!(!text.contains("://"), "{text:?}");
        prop_assert!(!text.to_ascii_lowercase().contains("www."), "{text:?}");
        let lower = text.to_ascii_lowercase();
        prop_assert!(
            !lower
                .match_indices("gh-")
                .any(|(at, _)| lower.get(at + 3..).is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))),
            "{text:?}"
        );
        prop_assert!(
            !text.chars().any(|c| c != '\n' && super::is_hidden(c)),
            "{text:?}"
        );
        prop_assert!(
            !text.lines().any(|line| line.starts_with(char::is_whitespace)),
            "{text:?}"
        );
    }
}

#[test]
fn code_is_fenced_with_its_info_string() {
    assert_eq!(
        code_block("let x = 1;", Info::Rust),
        "```rust\nlet x = 1;\n```"
    );
}

#[test]
fn the_code_fence_is_longer_than_any_backtick_run_inside() {
    assert_eq!(
        code_block("a ```` b\n```", Info::Text),
        "`````text\na ```` b\n```\n`````"
    );
}

#[test]
fn code_keeps_tabs_and_replaces_other_hidden_characters() {
    assert_eq!(
        code_block("\tx\u{202e}\r\ny\rz", Info::Text),
        "```text\n\tx\u{fffd}\ny\u{fffd}z\n```"
    );
}

#[test]
fn code_of_the_line_cap_is_kept_whole() {
    let code = vec!["x"; MAX_CODE_LINES].join("\n");

    assert_eq!(
        code_block(&code, Info::Text),
        format!("```text\n{code}\n```")
    );
}

#[test]
fn code_past_the_line_cap_is_clipped_and_marked() {
    let code = vec!["x"; MAX_CODE_LINES + 1].join("\n");

    let kept = vec!["x"; MAX_CODE_LINES].join("\n");
    assert_eq!(
        code_block(&code, Info::Text),
        format!("```text\n{kept}\n…\n```")
    );
}

#[test]
fn code_of_the_character_cap_is_kept_whole() {
    let code = "x".repeat(MAX_CODE_CHARS);

    assert_eq!(
        code_block(&code, Info::Text),
        format!("```text\n{code}\n```")
    );
}

#[test]
fn code_past_the_character_cap_is_clipped_and_marked() {
    let code = "x".repeat(MAX_CODE_CHARS + 1);

    let kept = "x".repeat(MAX_CODE_CHARS);
    assert_eq!(
        code_block(&code, Info::Text),
        format!("```text\n{kept}\n…\n```")
    );
}

#[test]
fn info_strings_come_from_the_path_extension() {
    let infos = [
        "src/lib.rs",
        "Cargo.toml",
        "a/b.json",
        "ci.yml",
        "x.yaml",
        "run.sh",
        "README",
        "evil.suggestion",
        "dir.rs/file",
    ]
    .map(Info::for_path);

    assert_eq!(
        infos,
        [
            Info::Rust,
            Info::Toml,
            Info::Json,
            Info::Yaml,
            Info::Yaml,
            Info::Shell,
            Info::Text,
            Info::Text,
            Info::Text
        ]
    );
}

#[test]
fn no_info_string_is_suggestion() {
    let names = Info::ALL.map(Info::as_str);

    assert_eq!(names, ["text", "rust", "toml", "json", "yaml", "sh"]);
}

#[test]
fn repository_names_of_github_shape_parse() {
    assert_eq!(
        Repository::parse("Wet-Ink-Corporation/assay.rs_1"),
        Some(Repository {
            owner: "Wet-Ink-Corporation",
            name: "assay.rs_1"
        })
    );
}

#[test]
fn malformed_repository_names_are_refused() {
    let long = format!("o/{}", "n".repeat(101));
    let refused = [
        "",
        "owner",
        "/repo",
        "owner/",
        "a/b/c",
        "own er/r",
        "o.x/r",
        "o/..",
        "o/.",
        "o/r)",
        long.as_str(),
    ]
    .map(Repository::parse);

    assert_eq!(refused, [None; 11]);
}

fn head() -> CommitId {
    "0123456789abcdef0123456789abcdef01234567"
        .parse()
        .expect("40 lowercase hex digits")
}

fn repository() -> Repository<'static> {
    Repository::parse("owner/repo").expect("a valid repository")
}

const SEVEN: NonZeroU64 = NonZeroU64::new(7).expect("seven is not zero");

#[test]
fn a_permalink_points_at_the_line_at_the_head() {
    assert_eq!(
        permalink(repository(), &head(), "src/lib.rs", SEVEN),
        "[src\\/lib\\.rs\\:7](https://github.com/owner/repo/blob/\
         0123456789abcdef0123456789abcdef01234567/src/lib.rs#L7)"
    );
}

#[test]
fn a_permalink_encodes_every_byte_that_could_end_its_destination() {
    let link = permalink(repository(), &head(), "a b/(x)<y>é.rs", SEVEN);

    assert_eq!(
        link,
        "[a b\\/\\(x\\)\\<y\\>é\\.rs\\:7](https://github.com/owner/repo/blob/\
         0123456789abcdef0123456789abcdef01234567/a%20b/%28x%29%3Cy%3E%C3%A9.rs#L7)"
    );
}

#[test]
fn a_path_that_could_leave_the_tree_gets_no_link() {
    let texts = ["../../other/repo", "a/./b", "/etc/x", "a//b"]
        .map(|path| permalink(repository(), &head(), path, SEVEN));

    assert_eq!(
        texts,
        [
            r"\.\.\/\.\.\/other\/repo\:7",
            r"a\/\.\/b\:7",
            r"\/etc\/x\:7",
            r"a\/\/b\:7"
        ]
    );
}

#[test]
fn a_path_of_the_cap_is_linked() {
    let path = "a".repeat(MAX_PATH_BYTES);

    let link = permalink(repository(), &head(), &path, SEVEN);

    assert!(link.ends_with(&format!("/{path}#L7)")), "{link}");
}

#[test]
fn a_path_past_the_cap_gets_no_link() {
    let path = "a".repeat(MAX_PATH_BYTES + 1);

    let text = permalink(repository(), &head(), &path, SEVEN);

    assert!(!text.contains("https"), "{text}");
}

#[test]
fn a_path_with_a_hidden_character_gets_no_link() {
    let texts = ["src/a\nb.rs", "src/\u{202e}x.rs", "src/a\rb.rs"]
        .map(|path| permalink(repository(), &head(), path, SEVEN));

    assert_eq!(
        texts,
        [
            "src\\/a\nb\\.rs\\:7",
            "src\\/\u{fffd}x\\.rs\\:7",
            "src\\/a\u{fffd}b\\.rs\\:7"
        ]
    );
}

#[test]
fn a_repository_name_of_the_cap_parses() {
    let full_name = format!("{}/{}", "o".repeat(100), "n".repeat(100));

    assert!(Repository::parse(&full_name).is_some());
}
