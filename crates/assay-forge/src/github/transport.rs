//! The seam between the adapter and whatever carries a request to GitHub: `gh api` in
//! production, recorded responses in tests.

use std::thread;
use std::time::Duration;

/// Sends one HTTP request to the GitHub API, authenticated as the transport was built.
///
/// A transport neither retries nor follows pages; [`GitHub`](super::GitHub) does both. It must
/// bound the response it reads (H-13) and report a non-2xx status as a [`Response`], not as an
/// error: only a failure to get any response is [`Transport::Error`].
pub trait Transport {
    /// Why no response was received.
    type Error: std::error::Error + 'static;

    /// Sends `request` and returns GitHub's response, whatever its status.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when no response was received.
    fn send(&self, request: &Request<'_>) -> Result<Response, Self::Error>;
}

impl<T: Transport + ?Sized> Transport for &T {
    type Error = T::Error;

    fn send(&self, request: &Request<'_>) -> Result<Response, Self::Error> {
        (**self).send(request)
    }
}

/// One request to the API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request<'a> {
    /// The method.
    pub method: Method,
    /// The path and query relative to `https://api.github.com/`, already percent-encoded, with
    /// no leading slash: `repos/acme/widgets/check-runs/1`.
    pub path: &'a str,
    /// The JSON body, for a write or a GraphQL query.
    pub body: Option<&'a [u8]>,
}

impl<'a> Request<'a> {
    /// `GET path`.
    #[must_use]
    pub const fn get(path: &'a str) -> Self {
        Self {
            method: Method::Get,
            path,
            body: None,
        }
    }

    /// `POST path` with `body`.
    #[must_use]
    pub const fn post(path: &'a str, body: &'a [u8]) -> Self {
        Self {
            method: Method::Post,
            path,
            body: Some(body),
        }
    }

    /// `PATCH path` with `body`.
    #[must_use]
    pub const fn patch(path: &'a str, body: &'a [u8]) -> Self {
        Self {
            method: Method::Patch,
            path,
            body: Some(body),
        }
    }
}

/// An HTTP method the adapter uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    /// `GET`.
    Get,
    /// `POST`.
    Post,
    /// `PATCH`.
    Patch,
}

impl Method {
    /// The method's name on the wire.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Patch => "PATCH",
        }
    }
}

/// GitHub's response to one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// The HTTP status.
    pub status: u16,
    /// The headers, names in any case, in the order received.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Vec<u8>,
}

impl Response {
    /// The value of the first header named `name`, compared case-insensitively.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Waits between retries. A seam so that tests observe the waits instead of taking them
/// (TST-10).
pub trait Sleep {
    /// Blocks the calling thread for `duration`.
    fn sleep(&self, duration: Duration);
}

impl<S: Sleep + ?Sized> Sleep for &S {
    fn sleep(&self, duration: Duration) {
        (**self).sleep(duration);
    }
}

/// [`Sleep`] on the calling thread. The adapter is synchronous, so this blocks no async task.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ThreadSleep;

impl Sleep for ThreadSleep {
    fn sleep(&self, duration: Duration) {
        thread::sleep(duration);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_lookup_ignores_the_name_case() {
        let response = Response {
            status: 200,
            headers: vec![("Retry-After".to_owned(), "7".to_owned())],
            body: Vec::new(),
        };

        assert_eq!(response.header("retry-after"), Some("7"));
        assert_eq!(response.header("link"), None);
    }
}
