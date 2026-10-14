//! Path globs for tiering (`docs/architecture/04-pipeline.md` §2): a small, total matcher, so
//! the domain needs no glob crate.

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// The most alternatives one glob's braces may expand to.
const MAX_ALTERNATIVES: usize = 64;

/// Characters reserved for syntax this matcher does not implement, so a pattern using them is
/// refused rather than read literally.
const RESERVED: [char; 5] = ['?', '[', ']', '\\', '!'];

/// A path glob over `/`-separated repository paths, matched against the whole path.
///
/// `*` matches any run of characters within one segment (dot files included), a `**` segment
/// matches zero or more whole segments, and `{a,b}` offers alternatives within one segment.
/// `?`, `[`, `]`, `\` and `!` are reserved, and empty, `.` and `..` segments are refused.
///
/// # Examples
///
/// ```
/// use assay_domain::Glob;
///
/// let docs: Glob = "docs/**/*.{md,txt}".parse()?;
/// assert!(docs.matches("docs/guide/intro.md"));
/// assert!(!docs.matches("src/lib.rs"));
/// # Ok::<(), assay_domain::GlobError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glob {
    pattern: String,
    /// The pattern after brace expansion, each alternative split into segments.
    alternatives: Vec<Vec<Segment>>,
}

/// One `/`-separated part of an expanded pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    /// `**`: zero or more whole segments.
    AnyDepth,
    /// Literal bytes and `*` wildcards.
    Name(String),
}

/// How a glob is compared with a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reach {
    /// Case-sensitive, whole path only.
    Exact,
    /// ASCII case-insensitive, and a match on any ancestor directory counts: the fail-closed
    /// reading for lists that raise a tier.
    Wide,
}

/// Why a glob was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum GlobError {
    /// The pattern is empty.
    #[error("a glob may not be empty")]
    Empty,
    /// The pattern starts with `/`; repository paths are relative.
    #[error("a glob may not start with `/`")]
    Absolute,
    /// The pattern uses a reserved character.
    #[error("`{0}` is reserved in a glob")]
    Reserved(char),
    /// A `{` without its `}`, or a `}` without its `{`.
    #[error("a glob's braces are unbalanced")]
    UnbalancedBrace,
    /// A `{` inside braces.
    #[error("a glob's braces may not nest")]
    NestedBrace,
    /// A `/` inside braces.
    #[error("a glob's braces may not contain `/`")]
    SlashInBrace,
    /// The braces expand to more than 64 alternatives.
    #[error("a glob's braces expand to more than {MAX_ALTERNATIVES} alternatives")]
    TooManyAlternatives,
    /// An empty segment: `a//b`, or a trailing `/`.
    #[error("a glob may not have an empty segment")]
    EmptySegment,
    /// A `.` or `..` segment, which no repository path contains.
    #[error("a glob may not have a `.` or `..` segment")]
    DotSegment,
    /// `**` that is not a whole segment, such as `a**`.
    #[error("`**` must be a whole segment")]
    PartialDoubleStar,
}

impl Glob {
    /// A glob from a pattern this crate ships, compiled without validation; the unit tests
    /// assert that every shipped pattern validates.
    pub(crate) fn builtin(pattern: &str) -> Self {
        Self {
            pattern: pattern.to_owned(),
            alternatives: compile(pattern),
        }
    }

    /// The pattern as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.pattern
    }

    /// Whether `path` matches the whole pattern, case-sensitively.
    #[must_use]
    pub fn matches(&self, path: &str) -> bool {
        self.matches_with(path, Reach::Exact)
    }

    /// Whether `path`, or any directory above it, matches ignoring ASCII case: the reading for
    /// trust, risk and never-trivial lists, where a wider match fails closed.
    pub(crate) fn matches_wide(&self, path: &str) -> bool {
        self.matches_with(path, Reach::Wide)
    }

    fn matches_with(&self, path: &str, reach: Reach) -> bool {
        let segments: Vec<&str> = path.split('/').collect();
        self.alternatives.iter().any(|pattern| {
            let matched = matched_prefixes(pattern, &segments, reach);
            match reach {
                Reach::Exact => matched.last().copied().unwrap_or(false),
                Reach::Wide => matched.iter().skip(1).any(|&prefix| prefix),
            }
        })
    }
}

impl FromStr for Glob {
    type Err = GlobError;

    fn from_str(pattern: &str) -> Result<Self, Self::Err> {
        validate(pattern)?;
        Ok(Self::builtin(pattern))
    }
}

/// Checks the syntax `compile` relies on.
fn validate(pattern: &str) -> Result<(), GlobError> {
    if pattern.is_empty() {
        return Err(GlobError::Empty);
    }
    if pattern.starts_with('/') {
        return Err(GlobError::Absolute);
    }
    if let Some(reserved) = pattern.chars().find(|c| RESERVED.contains(c)) {
        return Err(GlobError::Reserved(reserved));
    }
    validate_braces(pattern)?;
    validate_segments(pattern, pattern)?;
    // Braces hold no `/`, so each alternative has the raw pattern's segments, one for one.
    expand_braces(pattern)
        .iter()
        .try_for_each(|alternative| validate_segments(pattern, alternative))
}

/// Every segment of `expanded` is non-empty, not `.` or `..`, and holds `**` only when the
/// same segment of the raw `pattern` is exactly `**` (so `{*,x}*` cannot make one).
fn validate_segments(pattern: &str, expanded: &str) -> Result<(), GlobError> {
    let mut raw = pattern.split('/');
    for segment in expanded.split('/') {
        let whole_double_star = raw.next() == Some("**");
        match segment {
            "" => return Err(GlobError::EmptySegment),
            "." | ".." => return Err(GlobError::DotSegment),
            _ if segment.contains("**") && !whole_double_star => {
                return Err(GlobError::PartialDoubleStar);
            }
            _ => {}
        }
    }
    Ok(())
}

/// Braces are balanced, flat, slash-free and expand to at most `MAX_ALTERNATIVES` patterns.
fn validate_braces(pattern: &str) -> Result<(), GlobError> {
    let mut expansions: usize = 1;
    let mut group: Option<usize> = None;
    for c in pattern.chars() {
        group = match (c, group) {
            ('{', Some(_)) => return Err(GlobError::NestedBrace),
            ('{', None) => Some(1),
            ('}', None) => return Err(GlobError::UnbalancedBrace),
            ('}', Some(choices)) => {
                expansions = expansions.saturating_mul(choices);
                None
            }
            ('/', Some(_)) => return Err(GlobError::SlashInBrace),
            (',', Some(choices)) => Some(choices.saturating_add(1)),
            (_, open) => open,
        };
    }
    if group.is_some() {
        return Err(GlobError::UnbalancedBrace);
    }
    if expansions > MAX_ALTERNATIVES {
        return Err(GlobError::TooManyAlternatives);
    }
    Ok(())
}

/// The pattern's alternatives, split into segments. Total: an unmatched brace is literal.
fn compile(pattern: &str) -> Vec<Vec<Segment>> {
    expand_braces(pattern)
        .iter()
        .map(|alternative| {
            alternative
                .split('/')
                .map(|segment| match segment {
                    "**" => Segment::AnyDepth,
                    name => Segment::Name(name.to_owned()),
                })
                .collect()
        })
        .collect()
}

/// Every pattern the braces of `pattern` stand for, at most `MAX_ALTERNATIVES` of them.
fn expand_braces(pattern: &str) -> Vec<String> {
    let Some((head, rest)) = pattern.split_once('{') else {
        return vec![pattern.to_owned()];
    };
    let Some((choices, tail)) = rest.split_once('}') else {
        return vec![pattern.to_owned()];
    };
    let tails = expand_braces(tail);
    choices
        .split(',')
        .flat_map(|choice| tails.iter().map(move |end| format!("{head}{choice}{end}")))
        .take(MAX_ALTERNATIVES)
        .collect()
}

/// For each `j` in `0..=path.len()`, whether `pattern` matches `path[..j]`: one forward pass of
/// dynamic programming over segment positions, so neither several `**` nor a deep path can
/// make matching exponential, and every prefix is answered at once.
fn matched_prefixes(pattern: &[Segment], path: &[&str], reach: Reach) -> Vec<bool> {
    // `matched[j]`: whether the pattern segments processed so far match `path[..j]`.
    let mut matched: Vec<bool> = (0..=path.len()).map(|j| j == 0).collect();
    for segment in pattern {
        matched = match segment {
            Segment::AnyDepth => matched
                .iter()
                .scan(false, |seen, &here| {
                    *seen = *seen || here;
                    Some(*seen)
                })
                .collect(),
            Segment::Name(name) => std::iter::once(false)
                .chain(matched.iter().zip(path).map(|(&before, part)| {
                    before && match_name(name.as_bytes(), part.as_bytes(), reach)
                }))
                .collect(),
        };
    }
    matched
}

/// Whether one segment pattern of literal bytes and `*` matches `name`: the linear
/// two-pointer wildcard match, retrying from the last `*` on a mismatch.
fn match_name(pattern: &[u8], name: &[u8], reach: Reach) -> bool {
    let same = |a: u8, b: u8| match reach {
        Reach::Exact => a == b,
        Reach::Wide => a.eq_ignore_ascii_case(&b),
    };
    let (mut p, mut n) = (0_usize, 0_usize);
    // After the last `*`: the pattern position following it and the name position it has
    // absorbed up to.
    let mut star: Option<(usize, usize)> = None;
    loop {
        match (pattern.get(p), name.get(n)) {
            (Some(b'*'), _) => {
                star = Some((p.saturating_add(1), n));
                p = p.saturating_add(1);
            }
            (Some(&want), Some(&got)) if same(want, got) => {
                p = p.saturating_add(1);
                n = n.saturating_add(1);
            }
            (None, None) => return true,
            _ => match star {
                Some((resume, absorbed)) if absorbed < name.len() => {
                    let absorbed = absorbed.saturating_add(1);
                    star = Some((resume, absorbed));
                    p = resume;
                    n = absorbed;
                }
                _ => return false,
            },
        }
    }
}

impl fmt::Display for Glob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.pattern)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glob(pattern: &str) -> Glob {
        pattern.parse().expect("a valid test glob")
    }

    #[test]
    fn literal_matches_only_the_same_path() {
        let readme = glob("README.md");

        assert_eq!(
            [
                readme.matches("README.md"),
                readme.matches("docs/README.md")
            ],
            [true, false]
        );
    }

    #[test]
    fn star_stays_within_one_segment() {
        let star = glob("docs/*.md");

        assert_eq!(
            [
                star.matches("docs/a.md"),
                star.matches("docs/.md"),
                star.matches("docs/x/a.md"),
                star.matches("docs/a.txt"),
            ],
            [true, true, false, false]
        );
    }

    #[test]
    fn double_star_matches_zero_or_more_segments() {
        let leading = glob("**/auth/**");

        assert_eq!(
            [
                leading.matches("auth/x.rs"),
                leading.matches("src/a/auth/b/x.rs"),
                leading.matches("auth"),
                leading.matches("authz/x.rs"),
            ],
            [true, true, true, false]
        );
    }

    #[test]
    fn braces_offer_alternatives_within_a_segment() {
        let images = glob("docs/**/*.{png,jpg,svg}");

        assert_eq!(
            [
                images.matches("docs/a.png"),
                images.matches("docs/x/y/b.svg"),
                images.matches("docs/a.gif"),
            ],
            [true, true, false]
        );
    }

    #[test]
    fn matching_is_case_sensitive() {
        assert!(!glob("README.md").matches("readme.md"));
    }

    #[test]
    fn wide_matching_ignores_ascii_case_and_counts_ancestor_directories() {
        let unsafe_glob = glob("**/unsafe*");

        assert_eq!(
            [
                unsafe_glob.matches_wide("src/Unsafe/mod.rs"),
                unsafe_glob.matches_wide("src/UNSAFE_impl.rs"),
                unsafe_glob.matches("src/unsafe/mod.rs"),
                unsafe_glob.matches_wide("src/safe/mod.rs"),
            ],
            [true, true, false, false]
        );
    }

    #[test]
    fn many_double_stars_match_without_blowing_up() {
        let pattern = glob("**/a/**/b/**/c/**/d");
        let path = vec!["x"; 4_000].join("/");

        assert!(!pattern.matches_wide(&path), "no `a` segment in the path");
    }

    #[test]
    fn unmatched_brace_in_a_shipped_pattern_is_literal() {
        assert!(Glob::builtin("a{b").matches("a{b"), "compile is total");
    }

    #[test]
    fn several_stars_in_a_segment_backtrack() {
        let pattern = glob("*.min.*");

        assert_eq!(
            [
                pattern.matches("app.min.js"),
                pattern.matches("a.b.min.c.d"),
                pattern.matches("app.js"),
            ],
            [true, true, false]
        );
    }

    #[test]
    fn refuses_malformed_patterns() {
        let cases = [
            ("", GlobError::Empty),
            ("/abs", GlobError::Absolute),
            ("a//b", GlobError::EmptySegment),
            ("a/", GlobError::EmptySegment),
            ("a/./b", GlobError::DotSegment),
            ("../a", GlobError::DotSegment),
            ("a**", GlobError::PartialDoubleStar),
            ("**b/c", GlobError::PartialDoubleStar),
            ("a?", GlobError::Reserved('?')),
            ("[ab]", GlobError::Reserved('[')),
            ("a]", GlobError::Reserved(']')),
            ("a\\b", GlobError::Reserved('\\')),
            ("!a", GlobError::Reserved('!')),
            ("{a,b", GlobError::UnbalancedBrace),
            ("a}", GlobError::UnbalancedBrace),
            ("{a,{b}}", GlobError::NestedBrace),
            ("{a,b/c}", GlobError::SlashInBrace),
            ("{*,x}*", GlobError::PartialDoubleStar),
            ("{.,x}/a", GlobError::DotSegment),
            ("{,x}/a", GlobError::EmptySegment),
            (
                "{a,b}{c,d}{e,f}{g,h}{i,j}{k,l}{m,n}",
                GlobError::TooManyAlternatives,
            ),
        ];

        let refused: Vec<_> = cases
            .iter()
            .map(|(pattern, _)| pattern.parse::<Glob>().err())
            .collect();

        let expected: Vec<_> = cases.iter().map(|(_, error)| Some(*error)).collect();
        assert_eq!(refused, expected);
    }
}
