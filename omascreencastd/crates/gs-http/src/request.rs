//! Parsing one HTTP/1.1 request.
//!
//! Deliberately narrow: the GameStream control protocol is a handful of `GET`
//! and `POST` requests with small bodies, so a full HTTP stack would be a large
//! dependency for a small, hostile-input-facing job. The rules enforced here are
//! the ones that matter for safety:
//!
//! - the request line must have three tokens and a known method;
//! - headers are capped in count and length, so a client cannot exhaust memory;
//! - `Content-Length` is capped, and a `Transfer-Encoding` is refused rather
//!   than half-supported;
//! - the body is read exactly once and never trusted before the length check.
//!
//! Everything else is tolerant: unknown headers are kept and ignored, and the
//! path is percent-decoded for the small set of characters a query uses.

use std::io::{BufRead, Read};

/// Largest request line this server accepts.
pub const MAX_REQUEST_LINE: usize = 8 * 1024;

/// Largest number of headers this server accepts.
pub const MAX_HEADERS: usize = 64;

/// Largest single header value this server accepts.
pub const MAX_HEADER_VALUE: usize = 8 * 1024;

/// Largest body this server accepts.
///
/// Pairing bodies are a few hundred bytes of base64; a megabyte is already two
/// orders of magnitude of slack.
pub const MAX_BODY: usize = 1024 * 1024;

/// One parsed request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// Uppercase method, e.g. `GET`.
    pub method: String,
    /// Path with the query string removed.
    pub path: String,
    /// Raw query string, without the leading `?`.
    pub query: String,
    /// Headers, lower-cased names.
    pub headers: Vec<(String, String)>,
    /// Request body, empty when there is none.
    pub body: Vec<u8>,
}

impl Request {
    /// A header value by lower-case name.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.as_str())
    }

    /// Query-string pairs, in wire order, with percent-escapes decoded.
    ///
    /// `+` is decoded to a space, matching how a form-encoded query is built.
    /// A pair with no `=` is returned with an empty value.
    #[must_use]
    pub fn query_pairs(&self) -> Vec<(String, String)> {
        self.query
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| match pair.split_once('=') {
                Some((key, value)) => (crate::query::decode(key), crate::query::decode(value)),
                None => (crate::query::decode(pair), String::new()),
            })
            .collect()
    }

    /// A query parameter by name, first occurrence wins.
    #[must_use]
    pub fn query_param(&self, name: &str) -> Option<String> {
        self.query_pairs()
            .into_iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
    }
}

/// Why a request could not be parsed.
///
/// `std::io::Error` is not `Clone` or `PartialEq`, so this type is neither;
/// tests match on the variant instead of comparing whole values.
#[derive(Debug, thiserror::Error)]
pub enum RequestError {
    /// The request line was over [`MAX_REQUEST_LINE`] or unreadable.
    #[error("request line is too long or unreadable")]
    Line,
    /// The request line did not have exactly three tokens.
    #[error("malformed request line")]
    Shape,
    /// A header line was too long, or there were too many.
    #[error("header limits exceeded")]
    Headers,
    /// `Content-Length` was present but not a number.
    #[error("malformed content length")]
    ContentLength,
    /// A `Transfer-Encoding` was present, which this server does not support.
    #[error("chunked requests are not supported")]
    Chunked,
    /// The body exceeded [`MAX_BODY`].
    #[error("request body too large")]
    Body,
    /// The body ended before `Content-Length` bytes arrived.
    #[error("request body truncated")]
    Truncated,
    /// The connection failed.
    #[error("request io: {0}")]
    Io(#[from] std::io::Error),
}

/// Read exactly one request from `reader`.
///
/// # Errors
///
/// Returns [`RequestError`] for a malformed, oversized, or truncated request.
/// The connection should be closed after any error; there is no attempt to
/// resynchronise.
pub fn read_request<R: BufRead>(reader: &mut R) -> Result<Request, RequestError> {
    let mut line = String::new();
    let count = reader
        .by_ref()
        .take(MAX_REQUEST_LINE as u64 + 1)
        .read_line(&mut line)?;
    if count == 0 || count > MAX_REQUEST_LINE {
        return Err(RequestError::Line);
    }
    let line = line.trim_end_matches(['\r', '\n']);
    let mut parts = line.split(' ');
    let method = parts
        .next()
        .ok_or(RequestError::Shape)?
        .to_ascii_uppercase();
    let target = parts.next().ok_or(RequestError::Shape)?;
    let version = parts.next().ok_or(RequestError::Shape)?;
    if parts.next().is_some() || !version.starts_with("HTTP/") || method.is_empty() {
        return Err(RequestError::Shape);
    }

    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path.to_owned(), query.to_owned()),
        None => (target.to_owned(), String::new()),
    };

    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        let count = reader
            .by_ref()
            .take(MAX_HEADER_VALUE as u64 + 1)
            .read_line(&mut header)?;
        if count > MAX_HEADER_VALUE {
            return Err(RequestError::Headers);
        }
        let header = header.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            if count == 0 {
                return Err(RequestError::Truncated);
            }
            break;
        }
        if headers.len() >= MAX_HEADERS {
            return Err(RequestError::Headers);
        }
        let (name, value) = header.split_once(':').ok_or(RequestError::Shape)?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
    }

    if headers.iter().any(|(name, _)| name == "transfer-encoding") {
        return Err(RequestError::Chunked);
    }

    let length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .map(|(_, value)| value.parse::<usize>())
        .transpose()
        .map_err(|_| RequestError::ContentLength)?
        .unwrap_or(0);
    if length > MAX_BODY {
        return Err(RequestError::Body);
    }
    let mut body = vec![0u8; length];
    if length > 0 {
        reader.read_exact(&mut body)?;
    } else {
        body.clear();
    }

    if path.is_empty() {
        return Err(RequestError::Shape);
    }

    Ok(Request {
        method,
        path,
        query,
        headers,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn parse(raw: &str) -> Result<Request, RequestError> {
        read_request(&mut Cursor::new(raw.as_bytes().to_vec()))
    }

    #[test]
    fn parses_a_get_with_a_query_string() {
        let request =
            parse("GET /launch?appid=1&mode=1920x1080x60 HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "/launch");
        assert_eq!(request.query_param("appid").as_deref(), Some("1"));
        assert_eq!(request.query_param("mode").as_deref(), Some("1920x1080x60"));
        assert_eq!(request.header("host"), Some("x"));
        assert!(request.body.is_empty());
    }

    #[test]
    fn parses_a_post_body_of_exactly_content_length() {
        let request =
            parse("POST /clientchallenge HTTP/1.1\r\nContent-Length: 4\r\n\r\nabcd").unwrap();
        assert_eq!(request.body, b"abcd");
        assert_eq!(request.header("content-length"), Some("4"));
    }

    #[test]
    fn header_names_are_case_folded() {
        let request = parse("GET / HTTP/1.1\r\nX-Custom: v\r\n\r\n").unwrap();
        assert_eq!(request.header("x-custom"), Some("v"));
    }

    #[test]
    fn rejects_malformed_request_lines() {
        for raw in [
            "GET\r\n\r\n",
            "GET /only-a-path\r\n\r\n",
            "GET / HTTP/1.1 extra\r\n\r\n",
            "GET / NOTHTTP/1.1\r\n\r\n",
        ] {
            assert!(parse(raw).is_err(), "{raw:?} should be rejected");
        }
    }

    #[test]
    fn rejects_oversized_request_lines() {
        let raw = format!("GET /{} HTTP/1.1\r\n\r\n", "a".repeat(MAX_REQUEST_LINE));
        assert!(matches!(parse(&raw), Err(RequestError::Line)));
    }

    #[test]
    fn rejects_chunked_and_oversized_bodies() {
        let chunked = parse("POST /x HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n");
        assert!(matches!(chunked, Err(RequestError::Chunked)));

        let raw = format!(
            "POST /x HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY + 1
        );
        assert!(matches!(parse(&raw), Err(RequestError::Body)));
    }

    #[test]
    fn rejects_a_truncated_body() {
        assert!(matches!(
            parse("POST /x HTTP/1.1\r\nContent-Length: 10\r\n\r\nshort"),
            Err(RequestError::Io(_)) | Err(RequestError::Truncated)
        ));
    }

    #[test]
    fn query_pairs_are_percent_decoded_including_plus() {
        let request = parse("GET /x?a=1%202&b=x+y&c HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!(
            request.query_pairs(),
            vec![
                (String::from("a"), String::from("1 2")),
                (String::from("b"), String::from("x y")),
                (String::from("c"), String::new()),
            ]
        );
    }
}
