//! Minimal HTTP/1.1 request parser + response formatter for the AirDrop
//! endpoints (`/Discover`, `/Ask`, `/Upload`). Kept as pure functions over
//! byte buffers so the routing/body-decoding logic is fully unit-testable
//! without sockets or TLS. The iOS-18 quirk (chunked bodies with no
//! Content-Length) is handled via the [`crate::chunked`] decoder.
//!
//! Reference: `opendrop-patch/server.py.patched` (`do_POST`, `_read_request_body`).

use crate::chunked::decode_chunked;
use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum HttpError {
    #[error("malformed request line")]
    BadRequestLine,
    #[error("header section too short / no terminator")]
    NoHeaderTerminator,
    #[error("malformed header line: {0:?}")]
    BadHeaderLine(String),
    #[error("body read error: {0}")]
    Body(String),
}

/// A parsed HTTP/1.1 request (method, path, headers, decoded body).
#[derive(Debug, Clone, PartialEq)]
pub struct HttpRequest {
    pub method: String,
    pub path: String,
    /// Header values keyed by lower-case name.
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl HttpRequest {
    /// Case-insensitive header lookup.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(&name.to_ascii_lowercase()).map(|s| s.as_str())
    }
}

/// An HTTP response to be serialised onto the wire.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
    /// Extra headers (Content-Length is auto-added). Keys are sent as-is.
    pub headers: Vec<(String, String)>,
}

impl HttpResponse {
    pub fn new(status: u16) -> Self {
        Self { status, body: Vec::new(), headers: Vec::new() }
    }
}

// ---- RED stubs --------------------------------------------------------------

/// Parse an HTTP/1.1 request from raw bytes (headers + body). Handles both
/// `Content-Length` and `Transfer-Encoding: chunked` bodies.
pub fn parse_request(raw: &[u8]) -> Result<HttpRequest, HttpError> {
    let sep = find_subslice(raw, b"\r\n\r\n").ok_or(HttpError::NoHeaderTerminator)?;
    let head = std::str::from_utf8(&raw[..sep]).map_err(|_| HttpError::BadRequestLine)?;
    let body_start = sep + 4;
    let mut lines = head.split("\r\n");
    let req_line = lines.next().ok_or(HttpError::BadRequestLine)?;
    let mut parts = req_line.splitn(3, ' ');
    let method = parts.next().ok_or(HttpError::BadRequestLine)?.to_string();
    let path = parts.next().ok_or(HttpError::BadRequestLine)?.to_string();
    // version ignored (assumed 1.1)

    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (k, v) = line.split_once(':').ok_or_else(|| HttpError::BadHeaderLine(line.into()))?;
        headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
    }

    let body = if let Some(te) = headers.get("transfer-encoding") {
        if te.to_ascii_lowercase().contains("chunked") {
            decode_chunked(&raw[body_start..]).map_err(|e| HttpError::Body(e.to_string()))?
        } else {
            Vec::new()
        }
    } else if let Some(cl) = headers.get("content-length") {
        let n: usize = cl.parse().map_err(|_| HttpError::Body(format!("bad Content-Length: {cl}")))?;
        if body_start + n > raw.len() {
            return Err(HttpError::Body("body shorter than Content-Length".into()));
        }
        raw[body_start..body_start + n].to_vec()
    } else {
        Vec::new()
    };

    Ok(HttpRequest { method, path, headers, body })
}

/// Format an [`HttpResponse`] as wire bytes (status line + headers + body).
///
/// Always emits `Connection: close` unless the caller already set a
/// `Connection` header. The server closes the TLS stream after every request
/// (AirDrop opens a fresh connection per POST), so advertising `close` makes
/// the HTTP/1.1 connection-state on the wire match what we actually do — an
/// HTTP/1.1 client otherwise assumes keep-alive and may try to pipeline.
pub fn format_response(resp: &HttpResponse) -> Vec<u8> {
    let reason = match resp.status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        406 => "Not Acceptable",
        100 => "Continue",
        _ => "OK",
    };
    let mut out = Vec::new();
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Length: {cl}\r\n",
        status = resp.status,
        cl = resp.body.len(),
    );
    out.extend_from_slice(head.as_bytes());
    for (k, v) in &resp.headers {
        out.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
    }
    // HTTP/1.1 defaults to keep-alive. We do NOT auto-add `Connection: close`:
    // the AirDrop sender runs Discover -> Ask -> Upload over a single persistent
    // connection, so closing after Discover/Ask drops the pipelined Upload
    // (this is why web-link sends — which pipeline the Upload — were lost while
    // file sends, which reconnect, worked). Handlers that genuinely end the
    // connection (/Upload success, 406) set `Connection: close` explicitly.
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(&resp.body);
    out
}

/// Format an [`HttpRequest`] as wire bytes (request line + headers + body).
/// Auto-adds a `Content-Length` header unless a `Transfer-Encoding` is set.
/// Used by the AirDrop *sender* to serialise POST requests onto the TLS stream.
pub fn format_request(req: &HttpRequest) -> Vec<u8> {
    let mut out = Vec::new();
    let line = format!("{} {} HTTP/1.1\r\n", req.method, req.path);
    out.extend_from_slice(line.as_bytes());

    let has_te = req.headers.keys().any(|k| k.eq_ignore_ascii_case("transfer-encoding"));
    for (k, v) in &req.headers {
        out.extend_from_slice(format!("{}: {}\r\n", title_case_header(k), v).as_bytes());
    }
    if !has_te {
        out.extend_from_slice(format!("Content-Length: {}\r\n", req.body.len()).as_bytes());
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(&req.body);
    out
}

/// Canonical HTTP header casing: "content-type" -> "Content-Type".
fn title_case_header(name: &str) -> String {
    name.split('-')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join("-")
}

/// Parse an HTTP/1.1 response from raw bytes (status line + headers + body).
/// Only reads a `Content-Length`-delimited body (AirDrop responses are small
/// binary plists sent with Content-Length). Returns the status, headers, body.
pub fn parse_response(raw: &[u8]) -> Result<HttpResponse, HttpError> {
    let sep = find_subslice(raw, b"\r\n\r\n").ok_or(HttpError::NoHeaderTerminator)?;
    let head = std::str::from_utf8(&raw[..sep]).map_err(|_| HttpError::BadRequestLine)?;
    let body_start = sep + 4;
    let mut lines = head.split("\r\n");
    let status_line = lines.next().ok_or(HttpError::BadRequestLine)?;
    // "HTTP/1.1 200 OK"
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .ok_or(HttpError::BadRequestLine)?
        .parse()
        .map_err(|_| HttpError::BadRequestLine)?;

    let mut headers = Vec::new();
    let mut content_length: Option<usize> = None;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (k, v) = line.split_once(':').ok_or_else(|| HttpError::BadHeaderLine(line.into()))?;
        let key = k.trim();
        let val = v.trim();
        if key.eq_ignore_ascii_case("content-length") {
            content_length = Some(val.parse().unwrap_or(0));
        }
        headers.push((key.to_string(), val.to_string()));
    }

    let chunked = headers.iter().any(|(k, v)| k.eq_ignore_ascii_case("transfer-encoding") && v.to_ascii_lowercase().contains("chunked"));
    let body = if chunked {
        decode_chunked(&raw[body_start..]).map_err(|e| HttpError::Body(e.to_string()))?
    } else { match content_length {
        Some(n) => {
            if body_start + n > raw.len() {
                return Err(HttpError::Body("response body shorter than Content-Length".into()));
            }
            raw[body_start..body_start + n].to_vec()
        }
        None => Vec::new(),
    }};

    Ok(HttpResponse { status, body, headers })
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_request_with_content_length() {
        let raw = b"POST /Discover HTTP/1.1\r\n\
                    Host: [fe80::1]:8771\r\n\
                    Content-Type: application/x-apple-plist\r\n\
                    Content-Length: 5\r\n\
                    \r\n\
                    hello";
        let req = parse_request(raw).unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/Discover");
        assert_eq!(req.header("content-type"), Some("application/x-apple-plist"));
        assert_eq!(req.body, b"hello");
    }

    #[test]
    fn parse_request_chunked_no_content_length() {
        // The iOS-18 quirk: chunked body, NO Content-Length header.
        let raw = b"POST /Ask HTTP/1.1\r\n\
                    Host: [fe80::1]:8771\r\n\
                    Transfer-Encoding: chunked\r\n\
                    \r\n\
                    5\r\nhello\r\n\
                    0\r\n\r\n";
        let req = parse_request(raw).unwrap();
        assert_eq!(req.path, "/Ask");
        assert!(req.header("content-length").is_none());
        assert_eq!(req.header("transfer-encoding"), Some("chunked"));
        assert_eq!(req.body, b"hello");
    }

    #[test]
    fn parse_request_get_with_empty_body() {
        let raw = b"GET / HTTP/1.1\r\nHost: example\r\n\r\n";
        let req = parse_request(raw).unwrap();
        assert_eq!(req.method, "GET");
        assert_eq!(req.path, "/");
        assert!(req.body.is_empty());
    }

    #[test]
    fn parse_request_preserves_header_case_insensitive_lookup() {
        let raw = b"POST /Upload HTTP/1.1\r\n\
                    Content-Type: application/x-dvzip\r\n\
                    CONTENT-LENGTH: 3\r\n\
                    \r\n\
                    abc";
        let req = parse_request(raw).unwrap();
        assert_eq!(req.header("content-type"), Some("application/x-dvzip"));
        assert_eq!(req.header("CONTENT-TYPE"), Some("application/x-dvzip"));
    }

    #[test]
    fn format_response_basic() {
        let mut resp = HttpResponse::new(200);
        resp.headers.push(("Content-Type".into(), "text/plain".into()));
        resp.body = b"ok".to_vec();
        let bytes = format_response(&resp);
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(text.contains("Content-Type: text/plain\r\n"));
        assert!(text.contains("Content-Length: 2\r\n"));
        // Keep-alive default: no Connection header unless explicitly set.
        assert!(!text.contains("Connection:"), "Connection not auto-added (HTTP/1.1 keep-alive default)");
        assert!(text.ends_with("\r\n\r\nok"));
    }

    #[test]
    fn format_response_no_body() {
        let resp = HttpResponse::new(404);
        let bytes = format_response(&resp);
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.starts_with("HTTP/1.1 404 "));
        assert!(text.contains("Content-Length: 0\r\n"));
        // No auto Connection: close — keep-alive by default.
        assert!(!text.contains("Connection:"));
    }

    #[test]
    fn format_response_emits_explicit_close() {
        let mut resp = HttpResponse::new(200);
        resp.headers.push(("Connection".into(), "close".into()));
        let bytes = format_response(&resp);
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.contains("Connection: close\r\n"));
    }

    #[test]
    fn format_response_respects_explicit_connection_header() {
        // If the caller set Connection explicitly, don't double-emit.
        let mut resp = HttpResponse::new(200);
        resp.headers.push(("Connection".into(), "keep-alive".into()));
        let bytes = format_response(&resp);
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.contains("Connection: keep-alive\r\n"));
        assert_eq!(
            text.matches("Connection:").count(),
            1,
            "no duplicate Connection header"
        );
    }

    // ---- format_request (sender side) ----

    fn req(method: &str, path: &str, headers: Vec<(&str, &str)>, body: Vec<u8>) -> HttpRequest {
        let mut h = HashMap::new();
        for (k, v) in headers {
            h.insert(k.to_ascii_lowercase(), v.to_string());
        }
        HttpRequest { method: method.into(), path: path.into(), headers: h, body }
    }

    #[test]
    fn format_request_post_with_body_adds_content_length() {
        let r = req(
            "POST",
            "/Discover",
            vec![("Host", "[fe80::1]:8771"), ("Content-Type", "application/x-apple-plist")],
            b"hello".to_vec(),
        );
        let bytes = format_request(&r);
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.starts_with("POST /Discover HTTP/1.1\r\n"));
        assert!(text.contains("Host: [fe80::1]:8771\r\n"));
        assert!(text.contains("Content-Type: application/x-apple-plist\r\n"));
        assert!(text.contains("Content-Length: 5\r\n"));
        assert!(text.ends_with("\r\n\r\nhello"));
    }

    #[test]
    fn format_request_skips_content_length_when_chunked() {
        // iOS-18 Discover/Ask: chunked body, NO Content-Length. The caller
        // sets Transfer-Encoding: chunked and is expected to have already
        // chunk-framed the body.
        let r = req(
            "POST",
            "/Ask",
            vec![("Transfer-Encoding", "chunked")],
            b"5\r\nhello\r\n0\r\n\r\n".to_vec(),
        );
        let bytes = format_request(&r);
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.contains("Transfer-Encoding: chunked\r\n"));
        assert!(!text.contains("content-length"));
    }

    // ---- parse_response (sender side) ----

    #[test]
    fn parse_response_extracts_status_headers_body() {
        let raw = b"HTTP/1.1 200 OK\r\n\
                    Content-Type: application/x-apple-plist\r\n\
                    Content-Length: 5\r\n\
                    \r\n\
                    hello";
        let resp = parse_response(raw).unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, b"hello");
    }

    #[test]
    fn parse_response_handles_error_status() {
        let raw = b"HTTP/1.1 406 Not Acceptable\r\nContent-Length: 0\r\n\r\n";
        let resp = parse_response(raw).unwrap();
        assert_eq!(resp.status, 406);
        assert!(resp.body.is_empty());
    }
}
