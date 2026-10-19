//! Sending requests: one attempt classified, retries with backoff (06 F8), JSON decoding
//! through the duplicate-key-refusing parse, and pages followed by `Link: rel="next"`.

use std::time::Duration;

use serde::de::DeserializeOwned;

use super::error::{ForgeError, Operation};
use super::transport::{
    ApiPath, InvalidPath, MAX_RESPONSE_BYTES, Request, Response, Sleep, Transport,
};

/// Retries after the first attempt of a call that keeps failing with a retryable status.
pub(crate) const RETRIES: u8 = 3;
/// The wait before the first retry; each later one doubles it.
const FIRST_WAIT: Duration = Duration::from_secs(1);
/// The longest wait accepted, from `Retry-After` or the doubling: a longer one gives up.
const MAX_WAIT: Duration = Duration::from_secs(60);
/// Items requested per page, GitHub's maximum.
pub(crate) const PER_PAGE: usize = 100;
/// The most items any list reads; a longer list is refused, never cut (H-13). Above GitHub's
/// 2500 comments per conversation.
pub const MAX_LIST_ITEMS: usize = 3000;
/// The most pages any list reads: `MAX_LIST_ITEMS` at `PER_PAGE`.
const MAX_PAGES: usize = MAX_LIST_ITEMS.div_ceil(PER_PAGE);
/// What every `Link` target must start with; the rest is the next page's path.
const API_ORIGIN: &str = "https://api.github.com/";

/// GitHub's REST and GraphQL API over a [`Transport`], with retries. Used directly to mint
/// installation tokens, before a repository is chosen; [`GitHub`](super::GitHub) wraps it for
/// one repository.
#[derive(Debug)]
pub struct Api<T, S> {
    transport: T,
    sleeper: S,
}

/// Why one attempt did not succeed.
pub(crate) enum Failed<E> {
    /// A retryable status: 5xx or a secondary rate limit, with GitHub's `Retry-After`.
    Retry {
        /// The HTTP status.
        status: u16,
        /// GitHub's requested wait, if it gave one.
        wait: Option<Duration>,
    },
    /// Not retried.
    Stop(ForgeError<E>),
}

impl<E> From<ForgeError<E>> for Failed<E> {
    fn from(error: ForgeError<E>) -> Self {
        Self::Stop(error)
    }
}

impl<T: Transport, S: Sleep> Api<T, S> {
    /// The API over `transport`, waiting between retries with `sleeper`.
    pub const fn new(transport: T, sleeper: S) -> Self {
        Self { transport, sleeper }
    }

    /// Sends `request` once and keeps a 2xx response.
    pub(crate) fn attempt(
        &self,
        operation: Operation,
        request: &Request<'_>,
    ) -> Result<Response, Failed<T::Error>> {
        let response = self
            .transport
            .send(request)
            .map_err(|source| ForgeError::Transport { operation, source })?;
        classify(operation, response)
    }

    /// Runs `attempt` (given the attempt's index) until it succeeds, stops, or has been retried
    /// [`RETRIES`] times, waiting between attempts.
    ///
    /// # Errors
    ///
    /// [`ForgeError::Exhausted`] when every attempt was retryable, or a wait would pass
    /// [`MAX_WAIT`]; the attempt's own error when it stopped.
    pub(crate) fn retrying<V>(
        &self,
        operation: Operation,
        mut attempt: impl FnMut(u8) -> Result<V, Failed<T::Error>>,
    ) -> Result<V, ForgeError<T::Error>> {
        let mut index = 0_u8;
        loop {
            let (status, wait) = match attempt(index) {
                Ok(value) => return Ok(value),
                Err(Failed::Stop(error)) => return Err(error),
                Err(Failed::Retry { status, wait }) => (status, wait),
            };
            let attempts = index.saturating_add(1);
            let exhausted = ForgeError::Exhausted {
                operation,
                status,
                attempts,
            };
            if index >= RETRIES {
                return Err(exhausted);
            }
            let wait = match wait.or_else(|| backoff(index)) {
                Some(wait) if wait <= MAX_WAIT => wait,
                _ => return Err(exhausted),
            };
            self.sleeper.sleep(wait);
            index = attempts;
        }
    }

    /// Sends a request that is safe to repeat (a read, a whole overwrite, a token mint) with
    /// retries.
    pub(crate) fn send(
        &self,
        operation: Operation,
        request: Request<'_>,
    ) -> Result<Response, ForgeError<T::Error>> {
        self.retrying(operation, |_| self.attempt(operation, &request))
    }

    /// Sends `request`, a POST that is not safe to repeat blindly: before each retry, `landed`
    /// is asked whether the earlier POST took effect after all, and if so nothing is sent
    /// again.
    pub(crate) fn post_once(
        &self,
        operation: Operation,
        request: Request<'_>,
        mut landed: impl FnMut() -> Result<bool, ForgeError<T::Error>>,
    ) -> Result<(), ForgeError<T::Error>> {
        self.retrying(operation, |attempt| {
            if attempt > 0 && landed()? {
                return Ok(());
            }
            self.attempt(operation, &request).map(drop)
        })
    }

    /// `GET path`, decoded as `D`.
    pub(crate) fn get<D: DeserializeOwned>(
        &self,
        operation: Operation,
        path: &ApiPath,
    ) -> Result<D, ForgeError<T::Error>> {
        let response = self.send(operation, Request::get(path))?;
        decode(operation, &response.body)
    }

    /// `GET path`, or `None` on 404.
    pub(crate) fn get_optional<D: DeserializeOwned>(
        &self,
        operation: Operation,
        path: &ApiPath,
    ) -> Result<Option<D>, ForgeError<T::Error>> {
        match self.get(operation, path) {
            Ok(value) => Ok(Some(value)),
            Err(ForgeError::Rejected { status: 404, .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Every item of the list at `path` (which carries `per_page`), following `Link:
    /// rel="next"`, and stopping early once `stop_at` items are read.
    ///
    /// # Errors
    ///
    /// [`ForgeError::TooMany`] past [`MAX_LIST_ITEMS`] items or [`MAX_PAGES`] pages, and
    /// [`ForgeError::Malformed`] for a next page outside the API.
    pub(crate) fn list<P: Page>(
        &self,
        operation: Operation,
        path: ApiPath,
        stop_at: Option<usize>,
    ) -> Result<Vec<P::Item>, ForgeError<T::Error>> {
        let too_many = ForgeError::TooMany {
            operation,
            cap: MAX_LIST_ITEMS,
        };
        let mut items = Vec::new();
        let mut next = Some(path);
        let mut pages = 0_usize;
        while let Some(path) = next.take() {
            if pages >= MAX_PAGES {
                return Err(too_many);
            }
            pages = pages.saturating_add(1);
            let response = self.send(operation, Request::get(&path))?;
            items.extend(decode::<P, T::Error>(operation, &response.body)?.items());
            if items.len() > MAX_LIST_ITEMS {
                return Err(too_many);
            }
            if stop_at.is_some_and(|limit| items.len() >= limit) {
                break;
            }
            next = match response.header("link") {
                Some(link) => {
                    next_page(link).map_err(|detail| ForgeError::Malformed { operation, detail })?
                }
                None => None,
            };
        }
        Ok(items)
    }
}

/// A page of a list response: a bare array or an object holding one.
pub(crate) trait Page: DeserializeOwned {
    /// What the list holds.
    type Item;
    /// The page's items.
    fn items(self) -> Vec<Self::Item>;
}

impl<D: DeserializeOwned> Page for Vec<D> {
    type Item = D;

    fn items(self) -> Vec<D> {
        self
    }
}

/// The wait before retry `index` (0 for the first): [`FIRST_WAIT`] doubled `index` times.
fn backoff(index: u8) -> Option<Duration> {
    let factor = 1_u32.checked_shl(u32::from(index))?;
    FIRST_WAIT.checked_mul(factor)
}

/// A 2xx response, or why not.
fn classify<E>(operation: Operation, response: Response) -> Result<Response, Failed<E>> {
    let status = response.status;
    if (200..300).contains(&status) {
        return Ok(response);
    }
    let wait = match response.header("retry-after") {
        Some(value) => Some(retry_after(value).ok_or(ForgeError::Malformed {
            operation,
            detail: "Retry-After is not a number of seconds",
        })?),
        None => None,
    };
    let retryable = (500..600).contains(&status)
        || status == 429
        || (status == 403 && (wait.is_some() || mentions_secondary_limit(&response.body)));
    if retryable {
        Err(Failed::Retry { status, wait })
    } else {
        Err(Failed::Stop(ForgeError::Rejected { operation, status }))
    }
}

/// `Retry-After` as delta-seconds. GitHub sends no HTTP-date form.
fn retry_after(value: &str) -> Option<Duration> {
    let value = value.trim();
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok().map(Duration::from_secs)
}

/// Whether a 403's body is GitHub's secondary-rate-limit message.
fn mentions_secondary_limit(body: &[u8]) -> bool {
    const NEEDLE: &[u8] = b"secondary rate limit";
    body.windows(NEEDLE.len())
        .any(|window| window.eq_ignore_ascii_case(NEEDLE))
}

/// Decodes `body` as `D`, refusing a body over [`MAX_RESPONSE_BYTES`] unparsed and a repeated
/// key anywhere.
pub(crate) fn decode<D: DeserializeOwned, E>(
    operation: Operation,
    body: &[u8],
) -> Result<D, ForgeError<E>> {
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(ForgeError::ResponseTooLarge {
            operation,
            cap: MAX_RESPONSE_BYTES,
        });
    }
    assay_app::parse_json_strict(body)
        .and_then(serde_json::from_value)
        .map_err(|source| ForgeError::Json { operation, source })
}

/// Encodes `value` as a request body.
pub(crate) fn encode<E>(
    operation: Operation,
    value: &serde_json::Value,
) -> Result<Vec<u8>, ForgeError<E>> {
    serde_json::to_vec(value).map_err(|source| ForgeError::Json { operation, source })
}

/// The path of the `rel="next"` target in a `Link` header, if any.
///
/// # Errors
///
/// When the header is not a list of `<url>; params`, or the next target is not under
/// [`API_ORIGIN`] or not an [`ApiPath`] after it.
fn next_page(link: &str) -> Result<Option<ApiPath>, &'static str> {
    const BAD: &str = "Link header is not a list of <url>; params";
    let mut rest = link;
    loop {
        rest = rest.trim_start_matches([' ', ',']);
        if rest.is_empty() {
            return Ok(None);
        }
        let target = rest.strip_prefix('<').ok_or(BAD)?;
        let (url, after) = target.split_once('>').ok_or(BAD)?;
        let (params, tail) = after.split_once(',').unwrap_or((after, ""));
        let is_next = params
            .split(';')
            .any(|param| param.trim() == "rel=\"next\"");
        if is_next {
            let path = url
                .strip_prefix(API_ORIGIN)
                .ok_or("next page is not on api.github.com")?;
            return ApiPath::new(path)
                .map(Some)
                .map_err(|InvalidPath| "next page is not an API path");
        }
        rest = tail;
    }
}

/// `text` percent-encoded for a path segment or query value: every byte but RFC 3986's
/// unreserved ones.
pub(crate) fn percent_encode(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(hex_digit(byte >> 4));
            encoded.push(hex_digit(byte & 0x0f));
        }
    }
    encoded
}

/// The uppercase hexadecimal digit of the low four bits of `nibble`.
fn hex_digit(nibble: u8) -> char {
    let nibble = nibble & 0x0f;
    char::from(if nibble < 10 {
        b'0' + nibble
    } else {
        b'A' + (nibble - 10)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_page_is_the_rel_next_path() {
        let link = "<https://api.github.com/repositories/1/pulls?page=2>; rel=\"next\", \
                    <https://api.github.com/repositories/1/pulls?page=75>; rel=\"last\"";

        assert_eq!(
            next_page(link),
            Ok(Some(
                ApiPath::new("repositories/1/pulls?page=2").expect("a plain path")
            ))
        );
    }

    #[test]
    fn a_last_page_has_no_next() {
        let link = "<https://api.github.com/x?page=1>; rel=\"prev\", \
                    <https://api.github.com/x?page=1>; rel=\"first\"";

        assert_eq!(next_page(link), Ok(None));
    }

    #[test]
    fn a_next_page_off_the_api_is_refused() {
        let link = "<https://evil.example/repos/a/b?page=2>; rel=\"next\"";

        assert_eq!(next_page(link), Err("next page is not on api.github.com"));
    }

    #[test]
    fn a_garbled_link_is_refused() {
        assert_eq!(
            next_page("https://api.github.com/x; rel=\"next\""),
            Err("Link header is not a list of <url>; params")
        );
    }

    #[test]
    fn backoff_doubles_from_one_second() {
        assert_eq!(
            [backoff(0), backoff(1), backoff(2)],
            [1, 2, 4].map(|seconds| Some(Duration::from_secs(seconds)))
        );
    }

    #[test]
    fn retry_after_is_whole_seconds_only() {
        assert_eq!(retry_after(" 30 "), Some(Duration::from_secs(30)));
        assert_eq!(retry_after("-1"), None);
        assert_eq!(retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
    }

    #[test]
    fn percent_encoding_keeps_only_unreserved_bytes() {
        assert_eq!(
            percent_encode("assay / verdict @release/1.x"),
            "assay%20%2F%20verdict%20%40release%2F1.x"
        );
        assert_eq!(percent_encode("é"), "%C3%A9");
    }

    proptest::proptest! {
        #[test]
        fn an_encoded_value_never_carries_a_separator(text in ".*") {
            let encoded = percent_encode(&text);
            proptest::prop_assert!(
                !encoded.contains(['/', '?', '&', '#', '=', ' ']),
                "{encoded}"
            );
        }
    }
}
