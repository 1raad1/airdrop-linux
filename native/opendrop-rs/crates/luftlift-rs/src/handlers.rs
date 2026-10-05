//! AirDrop protocol handlers — pure functions that route a parsed
//! [`crate::http::HttpRequest`] to the right action (`/Discover`, `/Ask`,
//! `/Upload`) and produce an [`crate::http::HttpResponse`] plus any received
//! files. I/O-free for easy testing. See `opendrop-patch/server.py.patched`
//! (`do_POST`, `handle_*`).

use crate::cpio::read_odc_cpio;
use crate::dvzip::dvzip_to_cpio;
use crate::http::{HttpRequest, HttpResponse};
use crate::plist_impl::{ReceiverConfig, build_ask_response, build_discover_response, parse_ask_request};
use tracing::{info, warn};

/// A file extracted from an `/Upload` body.
#[derive(Debug, Clone, PartialEq)]
pub struct ReceivedFile {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// The outcome of handling a request: the response to send, plus any files
/// received (empty for non-Upload routes).
#[derive(Debug)]
pub struct HandleResult {
    pub response: HttpResponse,
    pub received_files: Vec<ReceivedFile>,
}

/// Route a POST request to the matching AirDrop handler. Returns a 404
/// [`HttpResponse`] for unknown paths.
pub fn handle_post(req: &HttpRequest, cfg: &ReceiverConfig) -> HandleResult {
    match req.path.as_str() {
        "/Discover" => handle_discover(req, cfg),
        "/Ask" => handle_ask(req, cfg),
        "/Upload" => handle_upload(req),
        path => {
            warn!(path, "unknown POST path");
            HandleResult { response: HttpResponse::new(404), received_files: Vec::new() }
        }
    }
}

/// Decode an Upload body (dvzip or raw CPIO) into extracted file entries.
/// Returns the list of (name, bytes) found in the archive.
pub fn decode_upload_body(content_type: &str, body: &[u8]) -> Vec<ReceivedFile> {
    let ct = content_type.to_ascii_lowercase();
    let cpio_bytes: Vec<u8> = if ct == "application/x-dvzip" {
        dvzip_to_cpio(body)
    } else {
        body.to_vec()
    };
    read_odc_cpio(&cpio_bytes)
        .into_iter()
        .map(|e| ReceivedFile { name: e.name, bytes: e.data.to_vec() })
        .collect()
}

fn handle_discover(_req: &HttpRequest, cfg: &ReceiverConfig) -> HandleResult {
    info!("Discover request — appearing in sender picker");
    let mut resp = HttpResponse::new(200);
    resp.headers.push(("Content-Type".into(), "application/x-apple-plist".into()));
    resp.body = build_discover_response(cfg);
    HandleResult { response: resp, received_files: Vec::new() }
}

fn handle_ask(req: &HttpRequest, cfg: &ReceiverConfig) -> HandleResult {
    let ask = parse_ask_request(&req.body);
    match &ask {
        Ok(a) => info!(
            transfer_id = %a.transfer_id,
            transfer_type = %a.transfer_type,
            sender = %a.sender_computer_name,
            file_count = a.files.len(),
            "Ask request — auto-accepting"
        ),
        Err(e) => warn!(error = %e, "failed to parse Ask body"),
    }
    let mut resp = HttpResponse::new(200);
    resp.headers.push(("Content-Type".into(), "application/x-apple-plist".into()));
    resp.body = build_ask_response(cfg);
    HandleResult { response: resp, received_files: Vec::new() }
}

fn handle_upload(req: &HttpRequest) -> HandleResult {
    let ctype = req.header("content-type").unwrap_or("");
    if !ctype.eq_ignore_ascii_case("application/x-dvzip")
        && !ctype.eq_ignore_ascii_case("application/x-cpio")
    {
        warn!(content_type = ctype, "unsupported Upload content-type");
        // Match the reference 406: Content-Type application/x-cpio,
        // Content-Length 0, Connection close.
        let mut resp = HttpResponse::new(406);
        resp.headers.push(("Content-Type".into(), "application/x-cpio".into()));
        resp.headers.push(("Connection".into(), "close".into()));
        return HandleResult { response: resp, received_files: Vec::new() };
    }

    let files = decode_upload_body(ctype, &req.body);
    for f in &files {
        info!(file = %f.name, bytes = f.bytes.len(), "received file");
    }
    info!(count = files.len(), "Upload complete");

    let mut resp = HttpResponse::new(200);
    resp.headers.push(("Connection".into(), "close".into()));
    HandleResult { response: resp, received_files: files }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::HttpRequest;
    use crate::plist_impl::ReceiverConfig;
    use crate::{cpio::build_odc_cpio, dvzip::cpio_to_dvzip};
    use plist::{Dictionary, Value};
    use std::collections::HashMap;

    fn cfg() -> ReceiverConfig {
        ReceiverConfig {
            computer_name: "luftlift-pc".into(),
            computer_model: "MacBookPro".into(),
            record_data: None,
        }
    }

    fn post(path: &str, headers: Vec<(&str, &str)>, body: Vec<u8>) -> HttpRequest {
        let mut h = HashMap::new();
        for (k, v) in headers {
            h.insert(k.to_ascii_lowercase(), v.to_string());
        }
        HttpRequest { method: "POST".into(), path: path.into(), headers: h, body }
    }

    // ---- /Discover ----

    #[test]
    fn handle_discover_returns_200_with_receiver_plist() {
        let req = post("/Discover", vec![("Content-Type", "application/x-apple-plist")], Vec::new());
        let res = handle_post(&req, &cfg());
        assert_eq!(res.response.status, 200);
        assert!(!res.response.body.is_empty());
        assert_eq!(&res.response.body[..8], b"bplist00");
        let val = Value::from_reader(std::io::Cursor::new(&res.response.body[..])).unwrap();
        assert_eq!(
            val.as_dictionary().unwrap().get("ReceiverComputerName").unwrap().as_string(),
            Some("luftlift-pc"),
        );
    }

    // ---- /Ask ----

    #[test]
    fn handle_ask_returns_200_and_logs_transfer() {
        // Build a minimal Ask request body.
        let mut d = Dictionary::new();
        d.insert("TransferID".into(), Value::String("XYZ-123".into()));
        d.insert("TransferType".into(), Value::String("links".into()));
        d.insert("SenderComputerName".into(), Value::String("iPhone".into()));
        d.insert("SenderModelName".into(), Value::String("iPhone".into()));
        d.insert("BundleID".into(), Value::String("com.apple.mobilesafari".into()));
        let mut body = Vec::new();
        Value::Dictionary(d).to_writer_binary(&mut body).unwrap();

        let req = post("/Ask", vec![("Content-Type", "application/x-apple-plist")], body);
        let res = handle_post(&req, &cfg());
        assert_eq!(res.response.status, 200);
        let val = Value::from_reader(std::io::Cursor::new(&res.response.body[..])).unwrap();
        assert_eq!(
            val.as_dictionary().unwrap().get("ReceiverComputerName").unwrap().as_string(),
            Some("luftlift-pc"),
        );
    }

    // ---- /Upload decode helpers (reuse the public encoders) ----

    fn make_dvzip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        cpio_to_dvzip(&build_odc_cpio(entries))
    }

    #[test]
    fn decode_upload_body_dvzip_extracts_files() {
        let dvzip = make_dvzip(&[("photo.jpg", &[0xFF, 0xD8, 0xFF, 0xE0])]);
        let files = decode_upload_body("application/x-dvzip", &dvzip);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "photo.jpg");
        assert_eq!(files[0].bytes, vec![0xFF, 0xD8, 0xFF, 0xE0]);
    }

    #[test]
    fn decode_upload_body_cpio_extracts_files() {
        let cpio = build_odc_cpio(&[("hello.txt", b"world"), ("a.bin", &[0, 1, 2])]);
        let files = decode_upload_body("application/x-cpio", &cpio);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].name, "hello.txt");
        assert_eq!(files[0].bytes, b"world");
        assert_eq!(files[1].name, "a.bin");
    }

    // ---- /Upload full handler ----

    #[test]
    fn handle_upload_dvzip_returns_200_and_received_files() {
        let dvzip = make_dvzip(&[("doc.txt", b"hi")]);
        let req = post(
            "/Upload",
            vec![("Content-Type", "application/x-dvzip"), ("Transfer-Encoding", "chunked")],
            dvzip,
        );
        let res = handle_post(&req, &cfg());
        assert_eq!(res.response.status, 200);
        assert_eq!(res.received_files.len(), 1);
        assert_eq!(res.received_files[0].name, "doc.txt");
        assert_eq!(res.received_files[0].bytes, b"hi");
    }

    // ---- unknown path ----

    #[test]
    fn handle_unknown_path_returns_404() {
        let req = post("/Foo", vec![], Vec::new());
        let res = handle_post(&req, &cfg());
        assert_eq!(res.response.status, 404);
    }

    #[test]
    fn handle_upload_rejects_bad_content_type_with_406_and_cpio_content_type() {
        // The reference 406 carries Content-Type: application/x-cpio.
        let req = post(
            "/Upload",
            vec![("Content-Type", "text/plain"), ("Transfer-Encoding", "chunked")],
            Vec::new(),
        );
        let res = handle_post(&req, &cfg());
        assert_eq!(res.response.status, 406);
        let ct = res
            .response
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            .map(|(_, v)| v.as_str());
        assert_eq!(ct, Some("application/x-cpio"), "406 must advertise Content-Type: application/x-cpio");
    }

    // ---- Regression: real Apple-style /Upload decode (iPhone "." bug) ----
    //
    // A real iPhone sends an ODC CPIO with a LEADING "." directory entry
    // (mode 040755), then the real file(s). The dvzip is split across
    // MULTIPLE zlib blocks, and the chunked HTTP body arrives as MULTIPLE
    // chunks. The earlier code (a) silently truncated the chunked body at
    // the first read desync (unwrap_or(0) treated any parse failure as
    // last-chunk) and (b) returned the "." directory as a saveable file
    // (EISDIR). This test reproduces the real shape end-to-end and asserts
    // the real file is extracted byte-exact, the "." is filtered, and no
    // truncation occurs.

    #[test]
    fn decode_upload_apple_style_dot_dir_plus_file_multi_block_multi_chunk() {
        use crate::chunked::decode_chunked;
        use crate::cpio::build_odc_cpio_with_leading_dir;
        use crate::dvzip::cpio_to_dvzip_blocks;

        // 1. Build an Apple-style CPIO: leading "." dir + real file.
        let file_content: Vec<u8> = (0u8..200).collect();
        let entries: &[(&str, &[u8])] = &[("photo.jpg", &file_content)];
        let cpio = build_odc_cpio_with_leading_dir(entries, true);
        assert!(cpio.starts_with(b"070707"), "cpio must start with ODC magic");

        // 2. Frame as multi-block dvzip (4 zlib blocks, like the real fixture).
        let dvzip = cpio_to_dvzip_blocks(&cpio, 4);

        // 3. Frame the dvzip as an HTTP chunked body split across multiple
        //    chunks (simulating how the iPhone sends it).
        let chunk_size = dvzip.len() / 3 + 1;
        let mut chunked_body = Vec::new();
        for chunk in dvzip.chunks(chunk_size) {
            chunked_body.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
            chunked_body.extend_from_slice(chunk);
            chunked_body.extend_from_slice(b"\r\n");
        }
        chunked_body.extend_from_slice(b"0\r\n\r\n");

        // 4. Decode the chunked body (the pure path — simulates what
        //    parse_request does after read_chunked_stream reads the raw).
        let body = decode_chunked(&chunked_body).expect("chunked decode must not truncate");
        assert_eq!(body.len(), dvzip.len(), "no body truncation: full dvzip recovered");

        // 5. Decode through the handler's decode_upload_body.
        let files = decode_upload_body("application/x-dvzip", &body);

        // 6. Assert: the real file is extracted byte-exact, "." is filtered.
        assert_eq!(files.len(), 1, "only the real file, not the '.' directory");
        assert_eq!(files[0].name, "photo.jpg");
        assert_eq!(files[0].bytes, file_content, "file bytes must be exact");
        assert!(!files.iter().any(|f| f.name == "."), "no '.' directory entry");
    }
}
