//! HTTP/1.1 chunked transfer-encoding body decoder.
//!
//! The iOS-18 quirk that broke stock opendrop: `/Discover` and `/Ask` POSTs
//! arrive with `Transfer-Encoding: chunked` and **no** `Content-Length`. A
//! naive `int(Content-Length)` crashes. This module decodes the chunked body
//! (the bytes *after* the blank `\r\n` separating headers from body) into the
//! logical body bytes — a pure, I/O-free function for easy testing. See
//! `opendrop-patch/server.py.patched::_read_request_body` for the reference.
//!
//! Format (RFC 7230 §4.1):
//! ```text
//! chunked-body = *chunk last-chunk trailer-part CRLF
//! chunk        = chunk-size [ chunk-ext ] CRLF chunk-data CRLF
//! last-chunk   = 1*"0" [ chunk-ext ] CRLF
//! ```

use thiserror::Error;

/// Errors that can occur decoding a chunked body.
#[derive(Debug, PartialEq, Eq, Error)]
pub enum ChunkedError {
    #[error("chunk size line is not valid hex: {0:?}")]
    InvalidSizeLine(String),
    #[error("truncated body: needed {needed} more bytes after the size line")]
    Truncated { needed: usize },
    #[error("missing CRLF terminator")]
    MissingCrlf,
}

/// Decode an HTTP/1.1 chunked body into the logical body bytes.
///
/// `input` is the body portion *after* the header/data separator `\r\n\r\n`.
/// Stops at the last-chunk (`0\r\n`) and consumes any trailer lines. This is
/// the pure counterpart to the Python `_read_request_body` chunked branch.
pub fn decode_chunked(input: &[u8]) -> Result<Vec<u8>, ChunkedError> {
    let mut out = Vec::new();
    let mut pos = 0;
    loop {
        // Read the chunk-size line up to CRLF.
        let line_end = find_crlf(&input[pos..]).ok_or(ChunkedError::MissingCrlf)?;
        let size_line = &input[pos..pos + line_end];
        pos += line_end + 2; // consume the line + CRLF

        let size_str = core::str::from_utf8(size_line).map_err(|_| {
            ChunkedError::InvalidSizeLine(String::from_utf8_lossy(size_line).into_owned())
        })?;
        // Strip any chunk-extension (";name=value") — only the leading hex token is the size.
        let size_token = size_str.split(';').next().unwrap().trim();
        let chunk_size =
            usize::from_str_radix(size_token, 16).map_err(|_| ChunkedError::InvalidSizeLine(size_str.to_string()))?;

        if chunk_size == 0 {
            // last-chunk: consume optional trailer-part up to the final CRLF.
            break;
        }

        // chunk-data. Use checked_add: an attacker-supplied size near
        // usize::MAX would otherwise overflow pos + chunk_size — panicking on
        // the add (debug) or wrapping to a small value that slips past the
        // bounds check and panics on the slice (release). Either is a remote
        // crash, since this runs on the live server's untrusted request body.
        let end = pos
            .checked_add(chunk_size)
            .filter(|&e| e <= input.len())
            .ok_or(ChunkedError::Truncated {
                needed: chunk_size.saturating_sub(input.len() - pos),
            })?;
        out.extend_from_slice(&input[pos..end]);
        pos = end;

        // trailing CRLF must immediately follow the chunk-data, not appear
        // somewhere later (which find_crlf would scan forward to, silently
        // mis-decoding). Require it at exactly `pos`.
        if input.get(pos..pos + 2) != Some(b"\r\n") {
            return Err(ChunkedError::MissingCrlf);
        }
        pos += 2;
    }
    Ok(out)
}

/// Find the offset of the first `\r\n` in `s`, or `None`.
fn find_crlf(s: &[u8]) -> Option<usize> {
    s.windows(2).position(|w| w == b"\r\n")
}

#[cfg(test)]
mod tests {
    use super::{ChunkedError, decode_chunked};

    #[test]
    fn decode_chunked_single_chunk() {
        // RFC example: "Wiki" in one chunk.
        let body = b"4\r\nWiki\r\n0\r\n\r\n";
        assert_eq!(decode_chunked(body).unwrap(), b"Wiki");
    }

    #[test]
    fn decode_chunked_multiple_chunks() {
        // RFC 7230 §4.1 worked example.
        let body = b"4\r\nWiki\r\n6\r\npedia \r\n0\r\n\r\n";
        assert_eq!(decode_chunked(body).unwrap(), b"Wikipedia ");
    }

    #[test]
    fn decode_chunked_empty_body() {
        // No data, just the terminator — the no-Content-Length / empty case.
        let body = b"0\r\n\r\n";
        assert_eq!(decode_chunked(body).unwrap(), b"");
    }

    #[test]
    fn decode_chunked_ignores_chunk_extensions() {
        // chunk-ext after the size (e.g. ";foo=bar") must be ignored.
        let body = b"5;foo=bar\r\nhello\r\n0\r\n\r\n";
        assert_eq!(decode_chunked(body).unwrap(), b"hello");
    }

    #[test]
    fn decode_chunked_handles_trailers() {
        // Optional trailer-part after the last-chunk.
        let body = b"3\r\nabc\r\n0\r\nX-Trace: 42\r\n\r\n";
        assert_eq!(decode_chunked(body).unwrap(), b"abc");
    }

    #[test]
    fn decode_chunked_preserves_binary_bytes() {
        // AirDrop bodies contain binary plists / zlib — byte-exact matters.
        let data: Vec<u8> = (0u8..=255).collect();
        let hex = format!("{:x}", data.len());
        let mut body = Vec::new();
        body.extend_from_slice(hex.as_bytes());
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(&data);
        body.extend_from_slice(b"\r\n0\r\n\r\n");
        assert_eq!(decode_chunked(&body).unwrap(), data.as_slice());
    }

    #[test]
    fn decode_chunked_rejects_bad_hex_size() {
        let body = b"not-hex\r\nWiki\r\n0\r\n\r\n";
        assert!(matches!(
            decode_chunked(body),
            Err(ChunkedError::InvalidSizeLine(_))
        ));
    }

    #[test]
    fn decode_chunked_rejects_overflowing_size() {
        // A chunk size near usize::MAX must not overflow pos + chunk_size
        // (which panics on the add or the slice). Reject it as truncated.
        let body = b"ffffffffffffffff\r\nWiki\r\n0\r\n\r\n";
        assert!(matches!(
            decode_chunked(body),
            Err(ChunkedError::Truncated { .. })
        ));
    }

    #[test]
    fn decode_chunked_rejects_misaligned_chunk_crlf() {
        // The CRLF must immediately follow the chunk-data; extra bytes before
        // it are a malformed body, not silently skipped.
        let body = b"4\r\nWikiXX\r\n0\r\n\r\n";
        assert!(matches!(
            decode_chunked(body),
            Err(ChunkedError::MissingCrlf)
        ));
    }

    #[test]
    fn decode_chunked_detects_truncated_data() {
        // Claims 10 bytes but only provides 4 before EOF.
        let body = b"10\r\nWiki";
        assert!(matches!(
            decode_chunked(body),
            Err(ChunkedError::Truncated { .. })
        ));
    }
}
