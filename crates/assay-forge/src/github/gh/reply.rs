//! The strict reading of what `gh api --include` writes on stdout: the status line, the
//! headers, a blank line and the body.
//!
//! gh 2.x writes `<proto> <code> <reason>\n` (Go's `Fprintln`), one `Name: value\r\n` line per
//! header (values of a repeated header joined with `, `), `\r\n`, then the body unchanged
//! (no colour: `NO_COLOR` is set and stdout is not a terminal). Anything else is refused,
//! never guessed at (wi-b5affa).

use crate::github::Response;

/// The most bytes the status line, the headers and the blank line after them may take.
pub(super) const MAX_HEAD_BYTES: usize = 64 * 1024;

/// The response `output` describes; its body is `output`'s own buffer, not a copy.
///
/// # Errors
///
/// What is wrong with it, when it is not exactly the shape above or its head is longer than
/// [`MAX_HEAD_BYTES`].
pub(super) fn parse(mut output: Vec<u8>) -> Result<Response, &'static str> {
    let (mut response, head) = read_head(&output)?;
    output.drain(..head);
    response.body = output;
    Ok(response)
}

/// The response's status and headers, with no body yet, and the length of the head (through
/// its blank line).
fn read_head(output: &[u8]) -> Result<(Response, usize), &'static str> {
    let (status_line, mut rest) = split_line(output, b"\n").ok_or("no status line")?;
    let status = status(status_line)?;
    let mut headers = Vec::new();
    loop {
        let (line, after) = split_line(rest, b"\r\n").ok_or("the headers do not end")?;
        rest = after;
        let head = output.len().saturating_sub(rest.len());
        if head > MAX_HEAD_BYTES {
            return Err("the head is longer than its cap");
        }
        if line.is_empty() {
            let response = Response {
                status,
                headers,
                body: Vec::new(),
            };
            return Ok((response, head));
        }
        headers.push(header(line)?);
    }
}

/// The line before the first `end`, and what follows `end`.
fn split_line<'a>(text: &'a [u8], end: &[u8]) -> Option<(&'a [u8], &'a [u8])> {
    let at = text.windows(end.len()).position(|window| window == end)?;
    let (line, rest) = text.split_at_checked(at)?;
    Some((line, rest.get(end.len()..)?))
}

/// The code of `HTTP/<version> <code> <reason>`: a version of digits and dots, a code of three
/// digits from 100 to 599, a printable reason.
fn status(line: &[u8]) -> Result<u16, &'static str> {
    const BAD: &str = "the status line is not HTTP/<version> <code> <reason>";
    // Not UTF-8 is one more way of not being that line; the detail says which line it was.
    let line = str::from_utf8(line).map_err(|_not_utf8| BAD)?;
    let version = line.strip_prefix("HTTP/").ok_or(BAD)?;
    let (version, rest) = version.split_once(' ').ok_or(BAD)?;
    let (code, reason) = rest.split_once(' ').ok_or(BAD)?;
    let plain_version = !version.is_empty()
        && version.starts_with(|c: char| c.is_ascii_digit())
        && version.chars().all(|c| c.is_ascii_digit() || c == '.');
    let printable = reason
        .bytes()
        .all(|byte| byte == b' ' || byte.is_ascii_graphic());
    if !plain_version || !printable || code.len() != 3 || !code.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(BAD);
    }
    code.parse()
        .ok()
        .filter(|code| (100..=599).contains(code))
        .ok_or(BAD)
}

/// `Name: value`: a name of ASCII letters, digits and `-`, a value of printable ASCII, spaces
/// and tabs, possibly empty.
fn header(line: &[u8]) -> Result<(String, String), &'static str> {
    const BAD: &str = "a header line is not `Name: value` in plain ASCII";
    let line = str::from_utf8(line).map_err(|_not_utf8| BAD)?;
    let (name, value) = line.split_once(": ").ok_or(BAD)?;
    let plain_name = !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
    let plain_value = value
        .bytes()
        .all(|byte| byte == b' ' || byte == b'\t' || byte.is_ascii_graphic());
    if !plain_name || !plain_value {
        return Err(BAD);
    }
    Ok((name.to_owned(), value.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`super::parse`] of a copy of `output`.
    fn parse(output: &[u8]) -> Result<Response, &'static str> {
        super::parse(output.to_vec())
    }

    fn response(status: u16, headers: &[(&str, &str)], body: &[u8]) -> Response {
        Response {
            status,
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
            body: body.to_vec(),
        }
    }

    #[test]
    fn reads_the_status_the_headers_in_order_and_the_body() {
        let output = b"HTTP/1.1 200 OK\nLink: <https://api.github.com/x?page=2>; rel=\"next\"\r\nX-Oauth-Scopes: \r\nX-Ratelimit-Remaining: 14867\r\n\r\n[{\"id\":1}]\r\n";

        assert_eq!(
            parse(output),
            Ok(response(
                200,
                &[
                    ("Link", "<https://api.github.com/x?page=2>; rel=\"next\""),
                    ("X-Oauth-Scopes", ""),
                    ("X-Ratelimit-Remaining", "14867"),
                ],
                b"[{\"id\":1}]\r\n",
            ))
        );
    }

    #[test]
    fn reads_http_2_and_a_response_without_headers_or_body() {
        assert_eq!(
            parse(b"HTTP/2.0 204 No Content\n\r\n"),
            Ok(response(204, &[], b""))
        );
        assert_eq!(
            parse(b"HTTP/2.0 401 Unauthorized\nServer: github.com\r\n\r\n{}"),
            Ok(response(401, &[("Server", "github.com")], b"{}"))
        );
    }

    #[test]
    fn refuses_every_other_shape() {
        for output in [
            &b""[..],
            b"HTTP/1.1 200 OK\r\n\r\n",
            b"HTTP/1.1 200 OK",
            b"HTTPS/1.1 200 OK\n\r\n",
            b"HTTP/ 200 OK\n\r\n",
            b"HTTP/1.x 200 OK\n\r\n",
            b"HTTP/1.1 20 OK\n\r\n",
            b"HTTP/1.1 2000 OK\n\r\n",
            b"HTTP/1.1 099 Low\n\r\n",
            b"HTTP/1.1 600 High\n\r\n",
            b"HTTP/1.1 +20 OK\n\r\n",
            b"HTTP/1.1 200OK\n\r\n",
            b"HTTP/1.1 200 \x1b[1mOK\n\r\n",
            b"HTTP/1.1 200 OK\nServer:github.com\r\n\r\n",
            b"HTTP/1.1 200 OK\nServer: github.com\n\r\n",
            b"HTTP/1.1 200 OK\n: empty\r\n\r\n",
            b"HTTP/1.1 200 OK\nSer ver: x\r\n\r\n",
            b"HTTP/1.1 200 OK\n\x1b[1;34mServer\x1b[m: x\r\n\r\n",
            b"HTTP/1.1 200 OK\nServer: caf\xc3\xa9\r\n\r\n",
            b"HTTP/1.1 200 OK\nServer: a\rb\r\n\r\n",
            b"HTTP/1.1 200 OK\nServer: github.com\r\n",
            b"HTTP/1.1 200 OK\nServer: github.com\r\n{}",
        ] {
            assert!(
                parse(output).is_err(),
                "{:?}",
                String::from_utf8_lossy(output)
            );
        }
    }

    #[test]
    fn refuses_a_head_past_its_cap() {
        let mut long = b"HTTP/1.1 200 OK\nX-Long: ".to_vec();
        long.resize(MAX_HEAD_BYTES, b'a');
        long.extend_from_slice(b"\r\n\r\n");

        assert_eq!(parse(&long), Err("the head is longer than its cap"));

        let mut fits = b"HTTP/1.1 200 OK\nX-Long: ".to_vec();
        fits.resize(MAX_HEAD_BYTES.saturating_sub(4), b'a');
        fits.extend_from_slice(b"\r\n\r\nbody");
        assert_eq!(parse(&fits).map(|read| read.body), Ok(b"body".to_vec()));
    }
}
