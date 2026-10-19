//! The seam between the adapter and whatever carries a request to GitHub: `gh api` in
//! production, recorded responses in tests.

use std::fmt;
use std::thread;
use std::time::Duration;

/// Sends one HTTP request to the GitHub API, authenticated as the transport was built.
///
/// A transport neither retries nor follows pages; [`GitHub`](super::GitHub) does both. It must
/// bound what it waits for and reads (H-13): each request ends within a timeout the transport
/// sets, an expired one being [`Transport::Error`], and it stops reading a body past
/// [`MAX_RESPONSE_BYTES`] plus one byte, so that the adapter refuses it rather than buffering
/// an unbounded answer. It reports a non-2xx status as a [`Response`], not as an error: only a
/// failure to get any response is [`Transport::Error`].
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

/// The largest response body the adapter decodes: [`assay_app::MAX_DOCUMENT_BYTES`]. A larger
/// one is refused unparsed as [`ForgeError::ResponseTooLarge`](super::ForgeError).
pub const MAX_RESPONSE_BYTES: usize = assay_app::MAX_DOCUMENT_BYTES;

/// A path and query relative to `https://api.github.com/`, already percent-encoded:
/// `repos/acme/widgets/check-runs/1`.
///
/// Validated so that no path can leave the API or reach a transport's command line as a
/// flag: it is not empty, has no leading `/` or `-`, no `://`, no `..` segment, and no
/// control byte, space or backslash.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ApiPath(String);

/// Why a path was refused as an [`ApiPath`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("not a plain path relative to the GitHub API")]
pub struct InvalidPath;

impl ApiPath {
    /// `path`, when it is a plain relative API path.
    ///
    /// # Errors
    ///
    /// [`InvalidPath`] when it is empty, starts with `/` or `-`, contains `://` or a `..`
    /// segment, or contains a control byte, a space or a backslash.
    pub fn new(path: impl Into<String>) -> Result<Self, InvalidPath> {
        let path = path.into();
        let plain = path
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b'\\');
        if !plain
            || path.is_empty()
            || path.starts_with(['/', '-'])
            || path.contains("://")
            || path.split(['/', '?', '#']).any(|segment| segment == "..")
        {
            return Err(InvalidPath);
        }
        Ok(Self(path))
    }

    /// The path as sent.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ApiPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One request to the API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request<'a> {
    /// The method.
    pub method: Method,
    /// The path and query relative to `https://api.github.com/`.
    pub path: &'a ApiPath,
    /// The JSON body, for a write or a GraphQL query.
    pub body: Option<&'a [u8]>,
}

impl<'a> Request<'a> {
    /// `GET path`.
    #[must_use]
    pub const fn get(path: &'a ApiPath) -> Self {
        Self {
            method: Method::Get,
            path,
            body: None,
        }
    }

    /// `POST path` with `body`.
    #[must_use]
    pub const fn post(path: &'a ApiPath, body: &'a [u8]) -> Self {
        Self {
            method: Method::Post,
            path,
            body: Some(body),
        }
    }

    /// `PATCH path` with `body`.
    #[must_use]
    pub const fn patch(path: &'a ApiPath, body: &'a [u8]) -> Self {
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

    #[test]
    fn an_api_path_is_relative_and_plain() {
        let path = ApiPath::new("repos/acme/widgets/check-runs?check_name=a%20b&page=2")
            .expect("a plain relative path");

        assert_eq!(
            path.as_str(),
            "repos/acme/widgets/check-runs?check_name=a%20b&page=2"
        );
        assert_eq!(
            ApiPath::new("graphql").map(|path| path.to_string()),
            Ok("graphql".to_owned())
        );
    }

    #[test]
    fn an_api_path_refuses_what_could_leave_the_api_or_become_a_flag() {
        for text in [
            "",
            "/repos/acme/widgets",
            "-X",
            "--method=DELETE",
            "https://evil.example/x",
            "repos/https://evil.example",
            "repos/acme widgets",
            "repos/acme\nwidgets",
            "repos/acme/../users",
            "..",
            "repos\\acme",
        ] {
            assert_eq!(ApiPath::new(text), Err(InvalidPath), "{text:?}");
        }
    }
}
