//! Links in reviewer text and where they lead (04 §4 step 5).
//!
//! A URL written out (`https://…`, `www.…`) shows where it leads, so it may point at the
//! allowlist or the repository. A markdown link hides its target behind its text, so its target
//! must be an absolute `https` URL into the repository; reference links and their definitions,
//! whose targets sit elsewhere, are always foreign.
//!
//! Every URL is judged on the longest extent a renderer could read: a written-out one up to an
//! ASCII space, control or `<` (GFM autolinks), a link destination up to an ASCII space,
//! control or unbalanced `)` (`CommonMark`). Where the screen cannot be sure how GitHub reads
//! one, the text is flagged rather than modelled more closely (wi-b5affa): a URL holding a
//! non-ASCII character, a written-out URL cut by a character other than `)` or `>`, a
//! repository URL holding `&` (a character reference), a backtick anywhere between `](` and
//! the link's closing `)`, which a code span could claim instead, and any `]:`.

/// Hosts a written-out URL may point to besides the repository, with their subdomains.
const ALLOWED_HOSTS: [&str; 3] = ["rust-lang.org", "docs.rs", "owasp.org"];

/// Characters that end a URL in running text for some renderer or reader.
const TERMINATORS: [char; 10] = ['(', ')', '[', ']', '<', '>', '"', '\'', '`', '|'];

/// The most characters one URL may run to; a longer one is foreign.
const MAX_URL: usize = 2048;

/// The most characters read back from `://` for its scheme.
const MAX_SCHEME: usize = 32;

/// Whether `text` links outside where it may: `lower` is the text in lowercase, `prose_lower`
/// its prose (code removed) in lowercase, and `repository` is `owner/name`.
pub(super) fn has_foreign_link(lower: &str, prose_lower: &str, repository: &str) -> bool {
    let repository = repository.to_lowercase();
    // Only ASCII is judged: a renderer may end a URL at, or decode, anything else differently.
    let written = Links::new(lower, Scan::Written).any(|link| match link {
        Link::Absolute { scheme, url, clean } => {
            scheme != "https" || !clean || !url.is_ascii() || !is_allowed(url, &repository)
        }
        Link::Bare { url, clean } => !clean || !url.is_ascii() || !is_allowed(url, &repository),
        Link::Overlong => true,
        Link::Target { .. } | Link::Reference => false,
    });
    // Links are found in the whole text, before code spans are paired, so a backtick anywhere
    // between `](` and the link's closing `)` (title included), which a code span could claim
    // and so hide what follows, flags the text.
    let ambiguous = Links::new(lower, Scan::Enclosed).any(|link| match link {
        Link::Target { target } => target.contains('`'),
        Link::Overlong => true,
        Link::Absolute { .. } | Link::Bare { .. } | Link::Reference => false,
    });
    let markdown = Links::new(prose_lower, Scan::Markdown).any(|link| match link {
        Link::Target { target } => {
            !target.is_ascii()
                || !target
                    .strip_prefix("https://")
                    .is_some_and(|url| in_repository(url, &repository))
        }
        Link::Reference | Link::Overlong => true,
        Link::Absolute { .. } | Link::Bare { .. } => false,
    });
    // `]:` defines a link reference wherever a container lets it start a line. Definitions are
    // parsed before code spans, so it is looked for in the whole text, spans included.
    written || ambiguous || markdown || lower.contains("]:")
}

/// Whether `c` ends a URL for every renderer: an ASCII space or control character.
const fn is_url_end(c: char) -> bool {
    c == ' ' || c.is_ascii_control()
}

/// Whether a written-out URL, read up to `following` (the character after it, if any), has one
/// reading only: no character in it ends a URL for some reader except `)` or `>`, and it was
/// not cut short by `<`.
fn clean_cut(url: &str, following: Option<char>) -> bool {
    let cut = url
        .find(TERMINATORS)
        .and_then(|at| url.get(at..))
        .and_then(|from| from.chars().next());
    cut.is_none_or(|c| c == ')' || c == '>') && following.is_none_or(is_url_end)
}

/// Where a URL's extent ends. Only ASCII spaces and controls end one anywhere: other
/// whitespace is part of the URL, and its non-ASCII content flags it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Extent {
    /// A written-out URL (GFM autolink): at a space, a control or `<`.
    Autolink,
    /// An inline link destination: at a space, a control or an unbalanced `)`.
    Destination,
    /// A destination in angle brackets: at `>`, `<` or a control.
    Angled,
    /// A whole inline link after `](`, title included: at an unbalanced `)` only.
    Enclosed,
}

impl Extent {
    /// Whether `c` ends the extent, with `depth` the parentheses open so far.
    fn ends_at(self, c: char, depth: &mut usize) -> bool {
        match self {
            Self::Autolink => is_url_end(c) || c == '<',
            Self::Angled => c == '>' || c == '<' || c.is_ascii_control(),
            Self::Destination | Self::Enclosed => match c {
                '(' => {
                    *depth = depth.saturating_add(1);
                    false
                }
                ')' => match depth.checked_sub(1) {
                    Some(open) => {
                        *depth = open;
                        false
                    }
                    None => true,
                },
                c => self == Self::Destination && is_url_end(c),
            },
        }
    }
}

/// Which links a scan looks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scan {
    /// `scheme://` and `www.`, anywhere in the text.
    Written,
    /// `](` and `][`, in prose; a target is the destination.
    Markdown,
    /// `](` and `][`; a target is everything up to the link's closing `)`.
    Enclosed,
}

/// One link a scan found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Link<'t> {
    /// `scheme://url`; `clean` as [`clean_cut`] says.
    Absolute {
        scheme: &'t str,
        url: &'t str,
        clean: bool,
    },
    /// A `www.` host GitHub autolinks; `url` starts at `www.`.
    Bare { url: &'t str, clean: bool },
    /// A markdown link's destination, after `](` and leading spaces, without angle brackets.
    Target { target: &'t str },
    /// A reference link, `[text][label]`.
    Reference,
    /// A URL longer than [`MAX_URL`].
    Overlong,
}

/// A forward-only scan for links. The cursor never moves back, and a URL it reads is skipped
/// past, so each byte is searched once and read as part of a URL at most once more: the scan
/// is linear in the text. `touched` counts that work.
#[derive(Debug)]
struct Links<'t> {
    text: &'t str,
    scan: Scan,
    at: usize,
    touched: usize,
}

impl<'t> Links<'t> {
    const fn new(text: &'t str, scan: Scan) -> Self {
        Self {
            text,
            scan,
            at: 0,
            touched: 0,
        }
    }

    /// The URL starting at byte `start` up to where `extent` ends it, and the cursor after it;
    /// `None` for the URL if it is overlong.
    fn url_at(&mut self, start: usize, extent: Extent) -> Option<(Option<&'t str>, usize)> {
        let rest = self.text.get(start..)?;
        let mut length = rest.len();
        let mut fits = true;
        let mut depth = 0_usize;
        for (count, (index, c)) in rest.char_indices().enumerate() {
            if count >= MAX_URL {
                (length, fits) = (index, false);
                break;
            }
            if extent.ends_at(c, &mut depth) {
                length = index;
                break;
            }
        }
        self.touched = self.touched.saturating_add(length);
        let url = rest.get(..length)?;
        Some((fits.then_some(url), start.checked_add(length)?))
    }

    /// A written-out URL starting at byte `start`, as `make` builds it from the URL and whether
    /// it is [`clean_cut`], and the cursor after it.
    fn written_at(
        &mut self,
        start: usize,
        make: impl FnOnce(&'t str, bool) -> Link<'t>,
    ) -> Option<(Option<Link<'t>>, usize)> {
        let (url, next) = self.url_at(start, Extent::Autolink)?;
        let following = self.text.get(next..)?.chars().next();
        let link = url.map_or(Link::Overlong, |url| make(url, clean_cut(url, following)));
        Some((Some(link), next))
    }

    /// The scheme ending at byte `end`: up to [`MAX_SCHEME`] scheme characters before it.
    fn scheme_before(&mut self, end: usize) -> Option<&'t str> {
        let before = self.text.get(..end)?;
        let length: usize = before
            .chars()
            .rev()
            .take(MAX_SCHEME)
            .take_while(|&c| c.is_ascii_alphanumeric() || "+-.".contains(c))
            .map(char::len_utf8)
            .sum();
        self.touched = self.touched.saturating_add(length);
        before.get(before.len().saturating_sub(length)..)
    }

    /// The link at byte `start`, where a marker character was found, and the cursor after it.
    fn link_at(&mut self, start: usize) -> Option<(Option<Link<'t>>, usize)> {
        let from = self.text.get(start..)?;
        let link = match self.scan {
            Scan::Written if from.starts_with("://") => {
                let scheme = self.scheme_before(start)?;
                return self.written_at(start.checked_add(3)?, |url, clean| Link::Absolute {
                    scheme,
                    url,
                    clean,
                });
            }
            Scan::Written if from.starts_with("www.") && self.autolinked(start) => {
                return self.written_at(start, |url, clean| Link::Bare { url, clean });
            }
            Scan::Enclosed if from.starts_with("](") => {
                let (target, next) = self.url_at(start.checked_add(2)?, Extent::Enclosed)?;
                return Some((
                    Some(target.map_or(Link::Overlong, |target| Link::Target { target })),
                    next,
                ));
            }
            Scan::Markdown if from.starts_with("](") => {
                let after = start.checked_add(2)?;
                let spaces = self.text.get(after..)?;
                let skipped = spaces
                    .len()
                    .saturating_sub(spaces.trim_start_matches([' ', '\t']).len());
                self.touched = self.touched.saturating_add(skipped);
                let at = after.checked_add(skipped)?;
                let (at, extent) = if self.text.get(at..)?.starts_with('<') {
                    (at.checked_add(1)?, Extent::Angled)
                } else {
                    (at, Extent::Destination)
                };
                let (target, next) = self.url_at(at, extent)?;
                return Some((
                    Some(target.map_or(Link::Overlong, |target| Link::Target { target })),
                    next,
                ));
            }
            Scan::Markdown | Scan::Enclosed if from.starts_with("][") => Some(Link::Reference),
            Scan::Written | Scan::Markdown | Scan::Enclosed => None,
        };
        Some((link, start.checked_add(1)?))
    }

    /// Whether the `www.` at byte `start` begins a word, so GitHub autolinks it.
    fn autolinked(&self, start: usize) -> bool {
        self.text
            .get(..start)
            .and_then(|before| before.chars().next_back())
            .is_none_or(|c| !(c.is_alphanumeric() || "/.".contains(c)))
    }
}

impl<'t> Iterator for Links<'t> {
    type Item = Link<'t>;

    fn next(&mut self) -> Option<Link<'t>> {
        let markers: &[char] = match self.scan {
            Scan::Written => &[':', 'w'],
            Scan::Markdown | Scan::Enclosed => &[']'],
        };
        loop {
            let found = self.text.get(self.at..)?.find(markers)?;
            self.touched = self.touched.saturating_add(found.saturating_add(1));
            let start = self.at.checked_add(found)?;
            let (link, next) = self.link_at(start)?;
            self.at = next;
            if link.is_some() {
                return link;
            }
        }
    }
}

/// Whether `url` (after `https://`) names an allowed host, or a path in `repository` on GitHub.
/// The authority must hold only `[a-z0-9.-]` (`url` is lowercased): a `\`, `%` or any other
/// character can end or respell the host a browser reads, so credentials and ports are never
/// allowed either.
fn is_allowed(url: &str, repository: &str) -> bool {
    let (authority, _) = split_authority(url);
    let host = authority.trim_end_matches('.');
    let allowed_host = authority
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
        && ALLOWED_HOSTS.iter().any(|allowed| {
            host == *allowed
                || host
                    .strip_suffix(allowed)
                    .is_some_and(|sub| sub.ends_with('.'))
        });
    allowed_host || in_repository(url, repository)
}

/// Whether `url` (after `https://`) is `github.com/<repository>` or a path under it, after
/// resolving `.` and `..` segments as a browser would. A path holding `%` or `\`, or a URL
/// holding `&` anywhere, is refused, since each can spell a segment the resolution does not
/// see (`&#46;` is a character reference to `.`).
fn in_repository(url: &str, repository: &str) -> bool {
    let (authority, path) = split_authority(url);
    let path = path.split(['?', '#']).next().unwrap_or(path);
    if !matches!(authority, "github.com" | "www.github.com")
        || path.contains(['%', '\\'])
        || url.contains('&')
    {
        return false;
    }
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            segment => segments.push(segment),
        }
    }
    match (segments.as_slice(), repository.split_once('/')) {
        ([owner, name, ..], Some((repo_owner, repo_name))) => {
            owner == &repo_owner && name == &repo_name
        }
        _ => false,
    }
}

/// `url` split into its authority and the rest, which starts at `/`, `?` or `#`.
fn split_authority(url: &str) -> (&str, &str) {
    url.find(['/', '?', '#'])
        .and_then(|end| url.split_at_checked(end))
        .unwrap_or((url, ""))
}

#[cfg(test)]
mod tests {
    use super::{Link, Links, Scan, TERMINATORS, has_foreign_link, in_repository};

    const REPO: &str = "wet-ink-corporation/assay";

    /// Whether `text` (taken as prose) has a foreign link.
    fn foreign(text: &str) -> bool {
        let lower = text.to_lowercase();
        has_foreign_link(&lower, &lower, REPO)
    }

    #[test]
    fn flags_a_written_url_any_terminator_could_cut_or_extend() {
        let flagged = TERMINATORS.map(|terminator| {
            foreign(&format!(
                "See https://github.com/wet-ink-corporation/assay/{terminator}/../../../evil/repo"
            ))
        });

        assert_eq!(flagged, [true; 10]);
    }

    #[test]
    fn flags_an_allowed_url_cut_by_anything_but_a_closer() {
        let flagged =
            TERMINATORS.map(|terminator| foreign(&format!("https://docs.rs/x{terminator}y")));

        assert_eq!(
            flagged,
            [true, false, true, true, true, false, true, true, true, true]
        );
    }

    #[test]
    fn flags_a_destination_any_terminator_could_extend() {
        // `)` closes the destination inside the repository; what follows it is plain text.
        let flagged = TERMINATORS
            .into_iter()
            .filter(|&terminator| terminator != ')')
            .map(|terminator| {
                let target = format!(
                    "https://github.com/wet-ink-corporation/assay/{terminator}/../../../evil/repo"
                );
                foreign(&format!("[x]({target})"))
            })
            .collect::<Vec<_>>();

        assert_eq!(flagged, [true; 9]);
    }

    #[test]
    fn reads_a_destination_through_balanced_parentheses_and_angle_brackets() {
        let texts = [
            "[x](https://github.com/wet-ink-corporation/assay/'/../../../evil/repo)",
            "[x](https://github.com/wet-ink-corporation/assay/(a)/../../../evil/repo)",
            "[x](<https://github.com/wet-ink-corporation/assay/ /../../../evil/repo>)",
            "[x](https://github.com/wet-ink-corporation/assay/blob/main/x.rs)",
            "See https://docs.rs/x) and (https://docs.rs/y).",
        ];

        assert_eq!(texts.map(foreign), [true, true, true, false, false]);
    }

    #[test]
    fn flags_a_destination_holding_a_backtick() {
        assert!(foreign(
            "[x](https://github.com/wet-ink-corporation/assay/`)"
        ));
    }

    #[test]
    fn resolves_dot_segments_before_checking_the_repository() {
        let urls = [
            "github.com/wet-ink-corporation/assay/blob/../tree/main",
            "github.com/wet-ink-corporation/assay/../assay/x",
            "github.com/wet-ink-corporation/assay/../../evil/repo",
            "github.com/wet-ink-corporation/./evil/x",
            "github.com/wet-ink-corporation/assay/%2e%2e/%2e%2e/evil/x",
            "github.com/wet-ink-corporation/assay\\..\\..\\evil",
            "github.com@evil.example/wet-ink-corporation/assay",
        ];

        assert_eq!(
            urls.map(|url| in_repository(url, REPO)),
            [true, true, false, false, false, false, false]
        );
    }

    #[test]
    fn finds_links_in_order_and_skips_past_each() {
        let links: Vec<_> =
            Links::new("a https://x.y/z b www.q.r ftp://s", Scan::Written).collect();

        assert_eq!(
            links,
            [
                Link::Absolute {
                    scheme: "https",
                    url: "x.y/z",
                    clean: true
                },
                Link::Bare {
                    url: "www.q.r",
                    clean: true
                },
                Link::Absolute {
                    scheme: "ftp",
                    url: "s",
                    clean: true
                },
            ]
        );
    }

    #[test]
    fn reports_a_url_past_the_length_cap_as_overlong() {
        let long = format!("https://{}", "x".repeat(3000));

        let links: Vec<_> = Links::new(&long, Scan::Written).collect();

        assert_eq!(links.first(), Some(&Link::Overlong));
    }

    #[test]
    fn touches_each_byte_of_hostile_text_a_bounded_number_of_times() {
        let hostile = [
            ("https://x".repeat(20_000), Scan::Written),
            ("a://".repeat(20_000), Scan::Written),
            ("://".repeat(20_000), Scan::Written),
            ("www.".repeat(20_000), Scan::Written),
            ("w".repeat(60_000), Scan::Written),
            ("](".repeat(30_000), Scan::Markdown),
            ("](<".repeat(20_000), Scan::Markdown),
            ("][".repeat(30_000), Scan::Markdown),
        ];

        for (text, scan) in hostile {
            let mut links = Links::new(&text, scan);
            links.by_ref().for_each(drop);

            assert!(
                links.touched <= 2 * text.len(),
                "{} bytes touched for {} bytes of {:?}",
                links.touched,
                text.len(),
                text.get(..8)
            );
        }
    }
}
