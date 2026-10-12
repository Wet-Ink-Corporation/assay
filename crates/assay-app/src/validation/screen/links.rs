//! Links in reviewer text and where they lead (04 §4 step 5).
//!
//! A URL written out (`https://…`, `www.…`) shows where it leads, so it may point at the
//! allowlist or the repository. A markdown link hides its target behind its text, so its target
//! must be an absolute `https` URL into the repository; reference links and their definitions,
//! whose targets sit elsewhere, are always foreign.

/// Hosts a written-out URL may point to besides the repository, with their subdomains.
const ALLOWED_HOSTS: [&str; 3] = ["rust-lang.org", "docs.rs", "owasp.org"];

/// The most characters one URL may run to; a longer one is foreign.
const MAX_URL: usize = 2048;

/// The most characters read back from `://` for its scheme.
const MAX_SCHEME: usize = 32;

/// Whether `text` links outside where it may: `lower` is the text in lowercase, `prose_lower`
/// its prose (code removed) in lowercase, and `repository` is `owner/name`.
pub(super) fn has_foreign_link(lower: &str, prose_lower: &str, repository: &str) -> bool {
    let repository = repository.to_lowercase();
    let written = Links::new(lower, Scan::Written).any(|link| match link {
        Link::Absolute { scheme, url } => scheme != "https" || !is_allowed(url, &repository),
        Link::Bare { url } => !is_allowed(url, &repository),
        Link::Overlong => true,
        Link::Target { .. } | Link::Reference => false,
    });
    let markdown = Links::new(prose_lower, Scan::Markdown).any(|link| match link {
        Link::Target { target } => !target
            .strip_prefix("https://")
            .is_some_and(|url| in_repository(url, &repository)),
        Link::Reference | Link::Overlong => true,
        Link::Absolute { .. } | Link::Bare { .. } => false,
    });
    written || markdown || has_definition(prose_lower)
}

/// Which links a scan looks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scan {
    /// `scheme://` and `www.`, anywhere in the text.
    Written,
    /// `](` and `][`, in prose.
    Markdown,
}

/// One link a scan found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Link<'t> {
    /// `scheme://url`.
    Absolute { scheme: &'t str, url: &'t str },
    /// A `www.` host GitHub autolinks; `url` starts at `www.`.
    Bare { url: &'t str },
    /// A markdown link's target, after `](`, leading spaces and `<`.
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

    /// The URL starting at byte `start`, ending before the first character that ends a URL in
    /// running text, and the cursor after it; `None` for the URL if it is overlong.
    fn url_at(&mut self, start: usize) -> Option<(Option<&'t str>, usize)> {
        let rest = self.text.get(start..)?;
        let mut length = rest.len();
        let mut fits = true;
        for (count, (index, c)) in rest.char_indices().enumerate() {
            if ends_url(c) {
                length = index;
                break;
            }
            if count >= MAX_URL {
                (length, fits) = (index, false);
                break;
            }
        }
        self.touched = self.touched.saturating_add(length);
        let url = rest.get(..length)?;
        Some((fits.then_some(url), start.checked_add(length)?))
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
                let (url, next) = self.url_at(start.checked_add(3)?)?;
                let link = url.map_or(Link::Overlong, |url| Link::Absolute { scheme, url });
                return Some((Some(link), next));
            }
            Scan::Written if from.starts_with("www.") && self.autolinked(start) => {
                let (url, next) = self.url_at(start)?;
                return Some((
                    Some(url.map_or(Link::Overlong, |url| Link::Bare { url })),
                    next,
                ));
            }
            Scan::Markdown if from.starts_with("](") => {
                let after = start.checked_add(2)?;
                let spaces = self.text.get(after..)?;
                let skipped = spaces
                    .len()
                    .saturating_sub(spaces.trim_start_matches([' ', '<']).len());
                self.touched = self.touched.saturating_add(skipped);
                let (target, next) = self.url_at(after.checked_add(skipped)?)?;
                return Some((
                    Some(target.map_or(Link::Overlong, |target| Link::Target { target })),
                    next,
                ));
            }
            Scan::Markdown if from.starts_with("][") => Some(Link::Reference),
            Scan::Written | Scan::Markdown => None,
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
            Scan::Markdown => &[']'],
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

/// Whether a line of `prose` defines a link reference, `[label]: target`.
fn has_definition(prose: &str) -> bool {
    prose.lines().any(|line| {
        line.trim_start()
            .strip_prefix('[')
            .is_some_and(|rest| rest.contains("]:"))
    })
}

/// Whether `url` (after `https://`) names an allowed host, or a path in `repository` on GitHub.
/// Credentials and ports are never allowed.
fn is_allowed(url: &str, repository: &str) -> bool {
    let (authority, _) = split_authority(url);
    let host = authority.trim_end_matches('.');
    let allowed_host = !authority.contains(['@', ':'])
        && ALLOWED_HOSTS.iter().any(|allowed| {
            host == *allowed
                || host
                    .strip_suffix(allowed)
                    .is_some_and(|sub| sub.ends_with('.'))
        });
    allowed_host || in_repository(url, repository)
}

/// Whether `url` (after `https://`) is `github.com/<repository>` or a path under it, after
/// resolving `.` and `..` segments as a browser would. A path holding `%` or `\` is refused,
/// since either can spell a segment the resolution does not see.
fn in_repository(url: &str, repository: &str) -> bool {
    let (authority, path) = split_authority(url);
    let path = path.split(['?', '#']).next().unwrap_or(path);
    if !matches!(authority, "github.com" | "www.github.com") || path.contains(['%', '\\']) {
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

/// Whether `c` ends a URL in running text.
fn ends_url(c: char) -> bool {
    c.is_whitespace() || "()[]<>\"'`|".contains(c)
}

#[cfg(test)]
mod tests {
    use super::{Link, Links, Scan, in_repository};

    const REPO: &str = "wet-ink-corporation/assay";

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
                    url: "x.y/z"
                },
                Link::Bare { url: "www.q.r" },
                Link::Absolute {
                    scheme: "ftp",
                    url: "s"
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
