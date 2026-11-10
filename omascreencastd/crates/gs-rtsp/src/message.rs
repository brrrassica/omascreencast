//! RTSP message framing, parsing, and serialisation (M2-T1).
//!
//! RTSP is HTTP/1.1-shaped: a start line, a header block terminated by a blank
//! line, and an optional body whose length is fixed by `Content-Length`. This
//! module implements exactly that, plus the two things GameStream leans on:
//! `CSeq` correlation and header folding (a continuation line begins with
//! whitespace and appends to the previous header's value).
//!
//! # Framing
//!
//! [`parse_request`] and [`parse_response`] return the number of bytes they
//! consumed, so a caller reading a stream can advance past exactly one message
//! and hand the remainder back. A message that is not yet complete returns
//! [`RtspError::Incomplete`] rather than a fatal error, because a TCP read
//! boundary is not a message boundary: the caller should read more and retry.

use crate::Method;

/// The RTSP version token this host speaks.
pub const RTSP_VERSION: &str = "RTSP/1.0";

/// Largest single RTSP message this host will buffer.
///
/// A control message is small; the cap exists so a hostile or broken peer
/// cannot make the daemon allocate without bound.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// A case-insensitive, insertion-ordered header map.
///
/// Insertion order is preserved so a serialised message is byte-stable, which
/// makes a golden-trace comparison meaningful. Lookup is case-insensitive
/// because header names are, and a client that sends `cseq` must be understood
/// the same as one that sends `CSeq`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Headers {
    entries: Vec<(String, String)>,
}

impl Headers {
    /// An empty header map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The first value for `name`, case-insensitively.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Insert or replace a header, preserving the position of an existing one.
    pub fn insert(&mut self, name: impl Into<String>, value: impl Into<String>) {
        let name = name.into();
        let value = value.into();
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|(key, _)| key.eq_ignore_ascii_case(&name))
        {
            entry.1 = value;
        } else {
            self.entries.push((name, value));
        }
    }

    /// Remove a header, returning its value if it was present.
    pub fn remove(&mut self, name: &str) -> Option<String> {
        let index = self
            .entries
            .iter()
            .position(|(key, _)| key.eq_ignore_ascii_case(name))?;
        Some(self.entries.remove(index).1)
    }

    /// Iterate over `(name, value)` pairs in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
    }

    /// How many headers are present.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no headers are present.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// An RTSP request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The method.
    pub method: Method,
    /// The request URI, verbatim.
    pub uri: String,
    /// The version token, normally [`RTSP_VERSION`].
    pub version: String,
    /// The header block.
    pub headers: Headers,
    /// The body, empty when there is none.
    pub body: Vec<u8>,
}

impl Request {
    /// Build a request with the standard version and no body.
    #[must_use]
    pub fn new(method: Method, uri: impl Into<String>) -> Self {
        Self {
            method,
            uri: uri.into(),
            version: RTSP_VERSION.to_owned(),
            headers: Headers::new(),
            body: Vec::new(),
        }
    }

    /// The `CSeq` value, if present and numeric.
    #[must_use]
    pub fn cseq(&self) -> Option<u32> {
        self.headers.get("cseq").and_then(parse_cseq)
    }

    /// Serialise to the wire form, including a `Content-Length` when a body is
    /// present and the caller did not set one.
    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(self.method.as_str().as_bytes());
        out.push(b' ');
        out.extend_from_slice(self.uri.as_bytes());
        out.push(b' ');
        out.extend_from_slice(self.version.as_bytes());
        out.extend_from_slice(b"\r\n");
        write_headers(&mut out, &self.headers, self.body.len());
        out.extend_from_slice(&self.body);
        out
    }
}

/// An RTSP response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// The version token, normally [`RTSP_VERSION`].
    pub version: String,
    /// The numeric status code.
    pub status: u16,
    /// The reason phrase.
    pub reason: String,
    /// The header block.
    pub headers: Headers,
    /// The body, empty when there is none.
    pub body: Vec<u8>,
}

impl Response {
    /// Build a response with the standard version and no body.
    #[must_use]
    pub fn new(status: u16, reason: impl Into<String>) -> Self {
        Self {
            version: RTSP_VERSION.to_owned(),
            status,
            reason: reason.into(),
            headers: Headers::new(),
            body: Vec::new(),
        }
    }

    /// A `200 OK` response.
    #[must_use]
    pub fn ok() -> Self {
        Self::new(200, "OK")
    }

    /// Set the `CSeq` header, echoing the request's value.
    #[must_use]
    pub fn with_cseq(mut self, cseq: u32) -> Self {
        self.headers.insert("CSeq", cseq.to_string());
        self
    }

    /// The `CSeq` value, if present and numeric.
    #[must_use]
    pub fn cseq(&self) -> Option<u32> {
        self.headers.get("cseq").and_then(parse_cseq)
    }

    /// Serialise to the wire form, including a `Content-Length` when a body is
    /// present and the caller did not set one.
    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(self.version.as_bytes());
        out.push(b' ');
        out.extend_from_slice(self.status.to_string().as_bytes());
        out.push(b' ');
        out.extend_from_slice(self.reason.as_bytes());
        out.extend_from_slice(b"\r\n");
        write_headers(&mut out, &self.headers, self.body.len());
        out.extend_from_slice(&self.body);
        out
    }
}

/// Why an RTSP message could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RtspError {
    /// The buffer does not yet hold a whole message; read more and retry.
    #[error("incomplete RTSP message: more bytes are needed")]
    Incomplete,
    /// The header section was not valid UTF-8.
    #[error("RTSP header section is not valid UTF-8")]
    NotUtf8,
    /// The message was structurally invalid.
    #[error("malformed RTSP message: {0}")]
    Malformed(String),
    /// The method token is not one this host accepts.
    #[error("unknown RTSP method: {0}")]
    UnknownMethod(String),
    /// The message exceeds [`MAX_MESSAGE_BYTES`].
    #[error("RTSP message exceeds the {MAX_MESSAGE_BYTES}-byte limit")]
    TooLarge,
}

/// Parse a request from the front of `input`.
///
/// Returns the request and the number of bytes consumed.
///
/// # Errors
///
/// [`RtspError::Incomplete`] when more bytes are needed; otherwise a structural
/// error.
pub fn parse_request(input: &[u8]) -> Result<(Request, usize), RtspError> {
    let (start_line, headers, body_start) = split_message(input)?;
    let mut parts = start_line.splitn(3, ' ');
    let method_token = parts
        .next()
        .filter(|token| !token.is_empty())
        .ok_or_else(|| RtspError::Malformed(String::from("missing method")))?;
    let uri = parts
        .next()
        .ok_or_else(|| RtspError::Malformed(String::from("missing request URI")))?;
    let version = parts
        .next()
        .ok_or_else(|| RtspError::Malformed(String::from("missing RTSP version")))?;
    let method = Method::parse(method_token)
        .ok_or_else(|| RtspError::UnknownMethod(method_token.to_owned()))?;
    let (body, consumed) = take_body(input, body_start, &headers)?;
    Ok((
        Request {
            method,
            uri: uri.to_owned(),
            version: version.to_owned(),
            headers,
            body,
        },
        consumed,
    ))
}

/// Parse a response from the front of `input`.
///
/// Returns the response and the number of bytes consumed.
///
/// # Errors
///
/// [`RtspError::Incomplete`] when more bytes are needed; otherwise a structural
/// error.
pub fn parse_response(input: &[u8]) -> Result<(Response, usize), RtspError> {
    let (start_line, headers, body_start) = split_message(input)?;
    let mut parts = start_line.splitn(3, ' ');
    let version = parts
        .next()
        .filter(|token| !token.is_empty())
        .ok_or_else(|| RtspError::Malformed(String::from("missing RTSP version")))?;
    let status_token = parts
        .next()
        .ok_or_else(|| RtspError::Malformed(String::from("missing status code")))?;
    let status: u16 = status_token
        .parse()
        .map_err(|_| RtspError::Malformed(format!("bad status code: {status_token:?}")))?;
    let reason = parts.next().unwrap_or("").to_owned();
    let (body, consumed) = take_body(input, body_start, &headers)?;
    Ok((
        Response {
            version: version.to_owned(),
            status,
            reason,
            headers,
            body,
        },
        consumed,
    ))
}

/// Split a message into its start line, headers, and the body offset.
fn split_message(input: &[u8]) -> Result<(String, Headers, usize), RtspError> {
    if input.len() > MAX_MESSAGE_BYTES {
        return Err(RtspError::TooLarge);
    }
    let (block, body_start) = split_header_block(input)?;
    let text = std::str::from_utf8(block).map_err(|_| RtspError::NotUtf8)?;
    let mut lines = text.split('\n');
    let start_line = lines
        .next()
        .ok_or(RtspError::Incomplete)?
        .trim_end_matches('\r')
        .to_owned();
    let headers = parse_header_lines(lines)?;
    Ok((start_line, headers, body_start))
}

/// Find the end of the header block, returning the block and the body offset.
///
/// CRLF is the wire form; a bare LF is tolerated because a hand-written test
/// fixture or a lenient client should be diagnosable here rather than as a
/// mysterious hang.
fn split_header_block(input: &[u8]) -> Result<(&[u8], usize), RtspError> {
    if let Some(position) = find_subslice(input, b"\r\n\r\n") {
        return Ok((&input[..position], position + 4));
    }
    if let Some(position) = find_subslice(input, b"\n\n") {
        return Ok((&input[..position], position + 2));
    }
    Err(RtspError::Incomplete)
}

/// Parse header lines, applying folding to continuation lines.
fn parse_header_lines<'a>(lines: impl Iterator<Item = &'a str>) -> Result<Headers, RtspError> {
    let mut headers = Headers::new();
    for raw_line in lines {
        let line = raw_line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            // A folded continuation appends to the previous header's value.
            let Some((_, value)) = headers.entries.last_mut() else {
                return Err(RtspError::Malformed(String::from(
                    "header continuation with no preceding header",
                )));
            };
            value.push(' ');
            value.push_str(line.trim());
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(RtspError::Malformed(format!(
                "header line without a colon: {line:?}"
            )));
        };
        headers.insert(name.trim(), value.trim());
    }
    Ok(headers)
}

/// Read the body described by `Content-Length`, returning it and the total
/// number of bytes consumed.
fn take_body(
    input: &[u8],
    body_start: usize,
    headers: &Headers,
) -> Result<(Vec<u8>, usize), RtspError> {
    let length = match headers.get("content-length") {
        Some(value) => value
            .trim()
            .parse::<usize>()
            .map_err(|_| RtspError::Malformed(format!("bad Content-Length: {value:?}")))?,
        None => 0,
    };
    let end = body_start.checked_add(length).ok_or(RtspError::TooLarge)?;
    if end > MAX_MESSAGE_BYTES {
        return Err(RtspError::TooLarge);
    }
    if input.len() < end {
        return Err(RtspError::Incomplete);
    }
    Ok((input[body_start..end].to_vec(), end))
}

/// Write a header block, adding `Content-Length` when a body is present and the
/// caller did not already set one.
fn write_headers(out: &mut Vec<u8>, headers: &Headers, body_len: usize) {
    for (name, value) in headers.iter() {
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(value.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    if body_len > 0 && headers.get("content-length").is_none() {
        out.extend_from_slice(format!("Content-Length: {body_len}\r\n").as_bytes());
    }
    out.extend_from_slice(b"\r\n");
}

/// Parse a `CSeq` value, tolerating surrounding whitespace.
fn parse_cseq(value: &str) -> Option<u32> {
    value.trim().parse().ok()
}

/// Find the first occurrence of `needle` in `haystack`.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_describe_request() {
        let raw = b"DESCRIBE rtsp://192.168.1.2:48010 RTSP/1.0\r\n\
                    CSeq: 2\r\n\
                    User-Agent: Moonlight/1.0\r\n\
                    Accept: application/sdp\r\n\r\n";
        let (request, consumed) = parse_request(raw).unwrap();
        assert_eq!(consumed, raw.len());
        assert_eq!(request.method, Method::Describe);
        assert_eq!(request.uri, "rtsp://192.168.1.2:48010");
        assert_eq!(request.version, RTSP_VERSION);
        assert_eq!(request.cseq(), Some(2));
        assert_eq!(request.headers.get("accept"), Some("application/sdp"));
        assert!(request.body.is_empty());
    }

    #[test]
    fn parses_a_request_with_a_body() {
        let raw = b"SETUP rtsp://host/streamid=0 RTSP/1.0\r\n\
                    CSeq: 3\r\n\
                    Content-Length: 5\r\n\r\nhello";
        let (request, consumed) = parse_request(raw).unwrap();
        assert_eq!(consumed, raw.len());
        assert_eq!(request.body, b"hello");
    }

    #[test]
    fn header_lookup_is_case_insensitive() {
        let raw = b"OPTIONS * RTSP/1.0\r\ncseq: 1\r\n\r\n";
        let (request, _) = parse_request(raw).unwrap();
        assert_eq!(request.headers.get("CSeq"), Some("1"));
        assert_eq!(request.cseq(), Some(1));
    }

    #[test]
    fn folded_headers_are_joined() {
        let raw = b"DESCRIBE rtsp://host RTSP/1.0\r\n\
                    CSeq: 1\r\n\
                    X-Long: first\r\n\
                    \tsecond\r\n\
                    \tthird\r\n\r\n";
        let (request, _) = parse_request(raw).unwrap();
        assert_eq!(request.headers.get("x-long"), Some("first second third"));
    }

    #[test]
    fn an_incomplete_message_reports_incomplete() {
        // No blank line yet.
        assert_eq!(
            parse_request(b"DESCRIBE rtsp://host RTSP/1.0\r\nCSeq: 1\r\n"),
            Err(RtspError::Incomplete)
        );
        // Header block complete but the body is short.
        assert_eq!(
            parse_request(b"SETUP rtsp://host RTSP/1.0\r\nContent-Length: 10\r\n\r\nabc"),
            Err(RtspError::Incomplete)
        );
    }

    #[test]
    fn framing_consumes_exactly_one_message() {
        let first = b"OPTIONS * RTSP/1.0\r\nCSeq: 1\r\n\r\n";
        let second = b"DESCRIBE rtsp://host RTSP/1.0\r\nCSeq: 2\r\n\r\n";
        let mut buffer = Vec::new();
        buffer.extend_from_slice(first);
        buffer.extend_from_slice(second);

        let (request, consumed) = parse_request(&buffer).unwrap();
        assert_eq!(request.method, Method::Options);
        assert_eq!(consumed, first.len());

        let (request, consumed) = parse_request(&buffer[consumed..]).unwrap();
        assert_eq!(request.method, Method::Describe);
        assert_eq!(consumed, second.len());
    }

    #[test]
    fn request_round_trips_through_serialisation() {
        let mut request = Request::new(Method::Setup, "rtsp://host/streamid=0");
        request.headers.insert("CSeq", "4");
        request.headers.insert("Transport", "RTP/AVP/UDP;unicast");
        request.body = b"payload".to_vec();

        let wire = request.serialize();
        let (parsed, consumed) = parse_request(&wire).unwrap();
        assert_eq!(consumed, wire.len());
        assert_eq!(parsed.method, request.method);
        assert_eq!(parsed.uri, request.uri);
        assert_eq!(parsed.body, request.body);
        assert_eq!(parsed.headers.get("cseq"), Some("4"));
        // A body implies a Content-Length on the wire.
        assert!(String::from_utf8_lossy(&wire).contains("Content-Length: 7"));
        // Re-serialising the parsed message is byte-identical: the auto-added
        // Content-Length is now explicit, so the round trip is stable.
        assert_eq!(parsed.serialize(), wire);
    }

    #[test]
    fn response_round_trips_through_serialisation() {
        let mut response = Response::ok().with_cseq(7);
        response.headers.insert("Server", "omascreencast");
        response.body = b"v=0\r\n".to_vec();

        let wire = response.serialize();
        let (parsed, consumed) = parse_response(&wire).unwrap();
        assert_eq!(consumed, wire.len());
        assert_eq!(parsed.status, 200);
        assert_eq!(parsed.reason, "OK");
        assert_eq!(parsed.cseq(), Some(7));
        assert_eq!(parsed.body, response.body);
        assert_eq!(parsed.serialize(), wire);
    }

    #[test]
    fn parses_a_response_with_a_reason_phrase_containing_spaces() {
        let raw = b"RTSP/1.0 461 Unsupported Transport\r\nCSeq: 9\r\n\r\n";
        let (response, _) = parse_response(raw).unwrap();
        assert_eq!(response.status, 461);
        assert_eq!(response.reason, "Unsupported Transport");
    }

    #[test]
    fn unknown_methods_are_rejected() {
        let raw = b"FROBNICATE * RTSP/1.0\r\n\r\n";
        assert_eq!(
            parse_request(raw),
            Err(RtspError::UnknownMethod(String::from("FROBNICATE")))
        );
    }

    #[test]
    fn malformed_start_lines_are_rejected() {
        assert!(matches!(
            parse_request(b"DESCRIBE\r\n\r\n"),
            Err(RtspError::Malformed(_))
        ));
        assert!(matches!(
            parse_response(b"RTSP/1.0 not-a-number OK\r\n\r\n"),
            Err(RtspError::Malformed(_))
        ));
    }

    #[test]
    fn header_insert_replaces_in_place() {
        let mut headers = Headers::new();
        headers.insert("CSeq", "1");
        headers.insert("Server", "x");
        headers.insert("cseq", "2");
        assert_eq!(headers.len(), 2);
        assert_eq!(headers.get("CSeq"), Some("2"));
        // Order is preserved: CSeq is still first.
        let names: Vec<&str> = headers.iter().map(|(name, _)| name).collect();
        assert_eq!(names, vec!["CSeq", "Server"]);
    }

    #[test]
    fn oversized_messages_are_refused() {
        let mut raw = b"OPTIONS * RTSP/1.0\r\n".to_vec();
        raw.resize(MAX_MESSAGE_BYTES + 1, b'a');
        assert_eq!(parse_request(&raw), Err(RtspError::TooLarge));
    }
}
