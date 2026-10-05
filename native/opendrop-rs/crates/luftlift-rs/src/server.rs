//! AirDrop HTTPS server — the accept loop that ties TLS + HTTP parsing +
//! handlers together. The per-connection logic is extracted as a pure
//! function over `BufRead + Write` so it's unit-testable without sockets.
//!
//! Reference: `opendrop-patch/server.py.patched` (`start_server`, `do_POST`).

use crate::handlers::{HandleResult, handle_post};
use crate::http::{HttpRequest, format_response, parse_request};
use crate::introspect::IntrospectState;
use crate::plist_impl::ReceiverConfig;
use anyhow::Result;
use rustls::ServerConfig;
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

/// Handle a single HTTP request from `reader`, write the response to `writer`.
/// Pure (no sockets) — works with in-memory buffers for testing.
///
/// If the request carries `Expect: 100-continue`, an interim
/// `HTTP/1.1 100 Continue` line is written to `writer` *before* the body is
/// read — mirroring `opendrop-patch/server.py.patched::handle_upload`. Without
/// this, an Apple sender that waits for the 100 before shipping the body
/// deadlocks against a server blocked reading the body.
pub fn handle_one_connection(
    reader: &mut impl BufRead,
    writer: &mut impl Write,
    cfg: &ReceiverConfig,
) -> Result<HandleResult> {
    let raw = read_request_respecting_expect(reader, writer)?;
    let req = parse_request(&raw)?;
    let result = handle_post(&req, cfg);
    let response_bytes = format_response(&result.response);
    writer.write_all(&response_bytes)?;
    Ok(result)
}

/// Read a complete HTTP/1.1 request (headers + body) from a `BufRead`,
/// without handling `Expect: 100-continue`. Kept for callers/tests that don't
/// need the interim response.
pub fn read_full_request(reader: &mut impl BufRead) -> Result<Vec<u8>> {
    let header_bytes = read_headers(reader)?;
    let body = read_body(reader, &header_bytes)?;
    let mut full = header_bytes;
    full.extend_from_slice(&body);
    Ok(full)
}

/// Read a complete HTTP/1.1 request. If the headers carry
/// `Expect: 100-continue`, write `HTTP/1.1 100 Continue\r\n\r\n` to `writer`
/// **before** reading the body, so a peer waiting on the interim response
/// ships the body. This is the counterpart to the Python upload handler's
/// `send_response(100)` branch.
pub fn read_request_respecting_expect(
    reader: &mut impl BufRead,
    writer: &mut impl Write,
) -> Result<Vec<u8>> {
    let header_bytes = read_headers(reader)?;
    let head_str = std::str::from_utf8(&header_bytes).unwrap_or("");
    if let Some(expect) = parse_header_value(head_str, "expect") {
        if expect.eq_ignore_ascii_case("100-continue") {
            writer.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
            // CRITICAL: flush so the 100 Continue actually reaches the peer.
            // On the TLS path rustls buffers writes internally; without an
            // explicit flush the 100 Continue is never sent, the Apple sender
            // waits forever for it, and the connection deadlocks.
            writer.flush()?;
        }
    }
    let body = read_body(reader, &header_bytes)?;
    let mut full = header_bytes;
    full.extend_from_slice(&body);
    Ok(full)
}

/// Read the HTTP/1.1 header section up to and including the `\r\n\r\n`
/// terminator. Returns the raw header bytes (including the terminator).
fn read_headers(reader: &mut impl BufRead) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    loop {
        let mut line = Vec::new();
        let n = reader.read_until(b'\n', &mut line)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&line);
        if buf.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    if buf.is_empty() {
        anyhow::bail!("connection closed before headers complete");
    }
    Ok(buf)
}

/// Read the body following already-parsed `header_bytes`. Dispatches on
/// `Content-Length` (exact read) or `Transfer-Encoding: chunked` (raw chunk
/// framing preserved for downstream `parse_request`).
fn read_body(reader: &mut impl BufRead, header_bytes: &[u8]) -> Result<Vec<u8>> {
    let head_str = std::str::from_utf8(header_bytes).unwrap_or("");
    if let Some(cl_str) = parse_header_value(head_str, "content-length") {
        let len: usize = cl_str.parse().unwrap_or(0);
        let mut body = vec![0u8; len];
        reader.read_exact(&mut body)?;
        Ok(body)
    } else if let Some(te_str) = parse_header_value(head_str, "transfer-encoding") {
        if te_str.to_ascii_lowercase().contains("chunked") {
            Ok(read_chunked_stream(reader)?)
        } else {
            Ok(Vec::new())
        }
    } else {
        Ok(Vec::new())
    }
}

/// Bump the introspection counters for an incoming AirDrop request. Pure
/// (no I/O) so it's unit-testable without sockets. Decodes the `/Ask` body
/// for the sender name; if decoding fails the counter is still bumped, just
/// without updating `last_peer`.
pub fn bump_introspect_for_request(state: &IntrospectState, req: &HttpRequest) {
    match req.path.as_str() {
        "/Discover" => state.bump_discover(),
        "/Ask" => {
            let sender = crate::plist_impl::parse_ask_request(&req.body)
                .ok()
                .map(|a| a.sender_computer_name);
            state.bump_ask(sender.as_deref());
        }
        "/Upload" => state.bump_upload(),
        _ => {}
    }
}

/// Serve forever on the given listener. Each connection is handled in a
/// thread. If `output_dir` is set, received files are saved there.
///
/// `introspect` is the shared receiver-state snapshot bumped on every
/// `/Discover`, `/Ask`, `/Upload` POST so the loopback introspect API
/// (`GET /status`) can report how far each Apple sender has progressed.
///
/// If the `LUFTLIFT_DUMP_UPLOAD` env var is set to a directory, each /Upload
/// raw body is also written there as `upload-<unix_ts>-<sender>.bin` for
/// offline decode diffing against the Python reference (`dvzip.py`). The
/// sender name is tracked across the per-connection AirDrop flow via a
/// shared `last_sender` cell updated when /Ask arrives.
pub fn serve_forever(
    listener: TcpListener,
    tls_config: Arc<ServerConfig>,
    cfg: ReceiverConfig,
    output_dir: Option<PathBuf>,
    introspect: Arc<IntrospectState>,
) {
    let cfg = Arc::new(cfg);
    let output_dir = Arc::new(output_dir);
    let dump_dir = dump_upload_dir();
    let last_sender: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let approvals = Arc::new(Mutex::new(HashMap::<IpAddr, Instant>::new()));
    for stream in listener.incoming() {
        match stream {
            Ok(tcp_stream) => {
                let tls_config = tls_config.clone();
                let cfg = cfg.clone();
                let output_dir = output_dir.clone();
                let dump_dir = dump_dir.clone();
                let last_sender = last_sender.clone();
                let introspect = introspect.clone();
                let approvals = approvals.clone();
                thread::spawn(move || {
                    if let Err(e) = handle_tls_connection(
                        tcp_stream,
                        tls_config,
                        cfg,
                        output_dir,
                        dump_dir,
                        last_sender,
                        introspect,
                        approvals,
                    ) {
                        warn!(error = %e, "connection error");
                    }
                });
            }
            Err(e) => error!(error = %e, "accept error"),
        }
    }
}

fn handle_tls_connection(
    tcp_stream: TcpStream,
    tls_config: Arc<ServerConfig>,
    cfg: Arc<ReceiverConfig>,
    output_dir: Arc<Option<PathBuf>>,
    dump_dir: Option<PathBuf>,
    last_sender: Arc<Mutex<String>>,
    introspect: Arc<IntrospectState>,
    approvals: Arc<Mutex<HashMap<IpAddr, Instant>>>,
) -> Result<()> {
    let peer = tcp_stream.peer_addr()?.ip();
    tcp_stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    tcp_stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    let conn = rustls::ServerConnection::new(tls_config)?;
    let mut tls = rustls::StreamOwned::new(conn, tcp_stream);
    // Read headers, then body. The split lets us emit an interim
    // `HTTP/1.1 100 Continue` between them when the client sent
    // `Expect: 100-continue` — without that an Apple sender can deadlock
    // waiting for the 100 before shipping the body. We write through
    // `reader.get_mut()` (the underlying `&mut tls`) because `BufReader`
    // only buffers reads.
    let mut reader = BufReader::new(&mut tls);
    let mut served = 0usize;
    loop {
        // Read the next request on this (keep-alive) connection. A clean close
        // after we've already served ≥1 request is normal end-of-connection,
        // not an error.
        let header_bytes = match read_headers(&mut reader) {
            Ok(h) => h,
            Err(_) if served > 0 => return Ok(()),
            Err(e) => return Err(e),
        };
        let head_str = std::str::from_utf8(&header_bytes).unwrap_or("");
        let is_upload = head_str.lines().next().unwrap_or("").starts_with("POST /Upload ");
        info!(request = head_str.lines().next().unwrap_or(""),
              content_length = parse_header_value(head_str, "content-length").unwrap_or_default(),
              encoding = parse_header_value(head_str, "transfer-encoding").unwrap_or_default(),
              "received request headers");
        if let Some(expect) = parse_header_value(head_str, "expect") {
            if expect.eq_ignore_ascii_case("100-continue") {
                reader.get_mut().write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
                // CRITICAL: flush the 100 Continue through the TLS encryption
                // layer so it reaches the Apple sender. Without this, rustls
                // buffers the plaintext and the sender never sees the 100,
                // deadlocking the /Upload (sender waits for 100, server waits
                // for body).
                reader.get_mut().flush()?;
            }
        }
        let (req, upload_ended) = if is_upload {
            let (body, complete) = read_upload_stream(&mut reader, head_str)?;
            // Normalize the decoded body for the existing HTTP parser.
            let mut normalized = head_str.lines().filter(|line| !line.is_empty()
                && !line.to_ascii_lowercase().starts_with("content-length:")
                && !line.to_ascii_lowercase().starts_with("transfer-encoding:"))
                .collect::<Vec<_>>().join("\r\n").into_bytes();
            normalized.extend_from_slice(format!("\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes());
            normalized.extend_from_slice(&body);
            (parse_request(&normalized)?, complete)
        } else {
            let body = read_body(&mut reader, &header_bytes)?;
            let mut raw = header_bytes;
            raw.extend_from_slice(&body);
            (parse_request(&raw)?, true)
        };

        // Track the sender name across the AirDrop flow (Discover/Ask/Upload
        // share one keep-alive connection, or reconnect) so the raw-upload
        // dump can be named after the device that sent the preceding /Ask.
        if req.path == "/Ask" {
            if let Ok(ask) = crate::plist_impl::parse_ask_request(&req.body) {
                if !ask.sender_computer_name.is_empty() {
                    if let Ok(mut cell) = last_sender.lock() {
                        *cell = ask.sender_computer_name.clone();
                    }
                }
            }
        }
        let sender_snapshot = last_sender
            .lock()
            .map(|cell| cell.clone())
            .unwrap_or_default();

        // Bump the introspection counters before dispatching so /status
        // reflects how far the Apple sender got (Discover -> Ask -> Upload)
        // even if the handler errors out.
        bump_introspect_for_request(&introspect, &req);

        // If configured by the desktop launcher, require a local Accept click.
        // Remember it across TLS reconnects, and consume it on Upload.
        if let Some(program) = std::env::var_os("LUFTLIFT_CONFIRM") {
            if req.path == "/Ask" {
                let ask = crate::plist_impl::parse_ask_request(&req.body)?;
                let names = ask.files.iter().map(|f| f.file_name.as_str())
                    .take(8).collect::<Vec<_>>().join("\n");
                let message = format!("{} wants to send {} file(s):\n{}\n\nAccept this AirDrop transfer?",
                    ask.sender_computer_name, ask.files.len(), names);
                let accepted = std::process::Command::new(program)
                    .args(["--title", "AirDrop", "--yesno", &message])
                    .status().map(|s| s.success()).unwrap_or(false);
                if !accepted {
                    let response = crate::http::HttpResponse::new(403);
                    reader.get_mut().write_all(&format_response(&response))?;
                    reader.get_mut().flush()?;
                    return Ok(());
                }
                approvals.lock().unwrap().insert(peer, Instant::now());
            } else if req.path == "/Upload" {
                let approved = approvals.lock().unwrap().remove(&peer)
                    .map(|at| at.elapsed() < Duration::from_secs(180)).unwrap_or(false);
                if !approved {
                    let response = crate::http::HttpResponse::new(403);
                    reader.get_mut().write_all(&format_response(&response))?;
                    reader.get_mut().flush()?;
                    return Ok(());
                }
            }
        }
        let mut result = handle_post(&req, &cfg);
        if is_upload {
            let archive = if req.header("content-type").unwrap_or("").eq_ignore_ascii_case("application/x-dvzip") {
                crate::dvzip::dvzip_to_cpio_checked(&req.body).unwrap_or_default()
            } else { req.body.clone() };
            if !crate::cpio::odc_archive_complete(&archive) {
                warn!(bytes = req.body.len(), "upload archive incomplete; refusing success");
                result.received_files.clear();
                result.response = crate::http::HttpResponse::new(400);
                result.response.headers.push(("Connection".into(), "close".into()));
            } else if !upload_ended {
                info!(bytes = req.body.len(), "complete archive verified before HTTP ending");
            }
        }
        // Save before reporting success. Flatten archive paths and create new
        // files exclusively so transfers cannot overwrite existing files.
        for f in &result.received_files {
            if let Some(dir) = output_dir.as_ref() {
                match save_received_file(dir, &f.name, &f.bytes) {
                    Ok(path) => info!(path = %path.display(), "saved file"),
                    Err(e) => {
                        warn!(error = %e, "failed to save file");
                        result.response = crate::http::HttpResponse::new(500);
                    }
                }
            }
        }

        // Env-gated raw /Upload capture for offline decode diffing against the
        // Python reference (opendrop-patch/dvzip.py). Off by default.
        if req.path == "/Upload" {
            if let Some(dir) = dump_dir.as_ref() {
                dump_raw_upload(dir, &sender_snapshot, &req.body);
            }
        }
        // Also dump the raw /Ask body under the same env var — some sends carry
        // the payload in /Ask, so this is the only way to see what was shared.
        if req.path == "/Ask" {
            if let Some(dir) = dump_dir.as_ref() {
                dump_raw_body(dir, "ask", &sender_snapshot, &req.body);
            }
        }

        // Keep-alive decision: close only if the handler ended the connection
        // (/Upload success, 406 — they set `Connection: close`) or the client
        // asked to. Otherwise loop to read the pipelined next request — this is
        // what lets a web-link Upload arrive on the same connection as its Ask.
        let resp_closes = result.response.headers.iter().any(|(k, v)| {
            k.eq_ignore_ascii_case("connection") && v.eq_ignore_ascii_case("close")
        });
        let req_closes = req
            .header("connection")
            .map(|v| v.eq_ignore_ascii_case("close"))
            .unwrap_or(false);

        let response_bytes = format_response(&result.response);
        reader.get_mut().write_all(&response_bytes)?;
        reader.get_mut().flush()?;
        // Open once per successful transfer, after every file is saved and
        // the success response is sent. The desktop launcher opts into this.
        if is_upload && result.response.status == 200 && !result.received_files.is_empty() {
            if let (Some(dir), Some(program)) =
                (output_dir.as_ref(), std::env::var_os("LUFTLIFT_OPEN_RECEIVED_FOLDER"))
            {
                match std::process::Command::new(program)
                    .arg("--new-window").arg(dir)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                {
                    Ok(mut child) => { thread::spawn(move || { let _ = child.wait(); }); }
                    Err(e) => warn!(error = %e, "could not open received folder"),
                }
            }
        }
        served += 1;
        if resp_closes || req_closes {
            return Ok(());
        }
    }
}

/// Bind a TCP listener on `[::]:port` (IPv6, dual-stack).
pub fn bind_listener(port: u16) -> Result<TcpListener> {
    let addr = SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port);
    let listener = TcpListener::bind(addr)?;
    info!(port, "HTTPS listener bound");
    Ok(listener)
}

// ---- helpers ----

fn save_received_file(dir: &std::path::Path, name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    let basename = std::path::Path::new(name).file_name()
        .filter(|n| !n.is_empty())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid filename"))?;
    std::fs::create_dir_all(dir)?;
    for suffix in 0..10000 {
        let filename = if suffix == 0 { basename.to_os_string() } else {
            let base = std::path::Path::new(basename);
            let stem = base.file_stem().unwrap_or(basename).to_string_lossy();
            let extension = base.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
            format!("{stem} ({suffix}){extension}").into()
        };
        let path = dir.join(filename);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => { file.write_all(bytes)?; return Ok(path); }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "too many duplicate names"))
}

fn parse_header_value(headers: &str, name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    for line in headers.split("\r\n") {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().to_ascii_lowercase() == lower {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

/// Read an HTTP/1.1 chunked body from a `BufRead` into a byte buffer
/// preserving the raw chunk framing (so the downstream `parse_request` /
/// `decode_chunked` can re-decode it). Stops at the genuine `0` last-chunk.
///
/// Robustness note: a malformed chunk-size line (partial read, CRLF desync,
/// stray byte) MUST NOT be silently treated as the `0` last-chunk. The
/// earlier `unwrap_or(0)` here did exactly that: a single bad size line
/// truncated the whole body after the first chunk, dropping the real file
/// payload (only the leading "." directory entry survived). We now error
/// loudly so truncation is visible instead of silently corrupting the
/// upload.
/// Preserve received upload bytes if the sender stops before HTTP framing
/// ends. The caller verifies the full CPIO archive before saving/success.
fn read_upload_stream(reader: &mut impl BufRead, headers: &str) -> Result<(Vec<u8>, bool)> {
    let mut body = Vec::new();
    let mut probe = UploadArchiveProbe::new(headers);
    let chunked = parse_header_value(headers, "transfer-encoding")
        .map(|v| v.to_ascii_lowercase().contains("chunked")).unwrap_or(false);
    if !chunked {
        let length: usize = parse_header_value(headers, "content-length")
            .unwrap_or_else(|| "0".into()).parse()?;
        let complete = read_upload_segment_probed(reader, &mut body, length, Some(&mut probe))?;
        return Ok((body, complete));
    }
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => return Ok((body, false)),
            Ok(_) => (),
            Err(e) if upload_stream_interrupted(&e) => return Ok((body, false)),
            Err(e) => return Err(e.into()),
        }
        let length = usize::from_str_radix(line.trim().split(';').next().unwrap_or(""), 16)?;
        if length == 0 {
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => return Ok((body, false)),
                    Ok(_) if line == "\r\n" || line == "\n" => return Ok((body, true)),
                    Ok(_) => (),
                    Err(e) if upload_stream_interrupted(&e) => return Ok((body, false)),
                    Err(e) => return Err(e.into()),
                }
            }
        }
        if !read_upload_segment_probed(reader, &mut body, length, Some(&mut probe))? { return Ok((body, false)); }
        let mut ending = Vec::new();
        if !read_upload_segment(reader, &mut ending, 2)? { return Ok((body, false)); }
        anyhow::ensure!(ending == b"\r\n", "invalid upload chunk ending");
    }
}

fn upload_stream_interrupted(e: &std::io::Error) -> bool {
    matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
        | std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::UnexpectedEof)
}

fn read_upload_segment(reader: &mut impl BufRead, output: &mut Vec<u8>, length: usize) -> Result<bool> {
    read_upload_segment_probed(reader, output, length, None)
}

struct UploadArchiveProbe {
    dvzip: bool,
    position: usize,
    archive: Vec<u8>,
}

impl UploadArchiveProbe {
    fn new(headers: &str) -> Self {
        Self {
            dvzip: parse_header_value(headers, "content-type")
                .map(|s| s.eq_ignore_ascii_case("application/x-dvzip")).unwrap_or(false),
            position: 0,
            archive: Vec::new(),
        }
    }

    fn complete(&mut self, body: &[u8]) -> bool {
        if !self.dvzip { return crate::cpio::odc_archive_complete(body); }
        // Inflate each block once, as it becomes complete, rather than
        // re-decoding the whole upload after every incoming network read.
        while let Some(prefix) = body.get(self.position..self.position.saturating_add(4)) {
            let descriptor = u32::from_be_bytes(prefix.try_into().unwrap());
            let length = (descriptor & 0x7fffffff) as usize;
            let Some(end) = self.position.checked_add(4).and_then(|n| n.checked_add(length)) else { return false; };
            if end > body.len() { break; }
            let Ok(block) = crate::dvzip::dvzip_to_cpio_checked(&body[self.position..end]) else { return false; };
            self.archive.extend_from_slice(&block);
            self.position = end;
            if crate::cpio::odc_archive_complete(&self.archive) { return true; }
        }
        false
    }
}

fn read_upload_segment_probed(reader: &mut impl BufRead, output: &mut Vec<u8>, length: usize,
    mut probe: Option<&mut UploadArchiveProbe>) -> Result<bool> {
    let mut remaining = length;
    let mut buffer = [0u8; 65536];
    while remaining > 0 {
        let limit = remaining.min(buffer.len());
        match reader.read(&mut buffer[..limit]) {
            Ok(0) => return Ok(false),
            Ok(n) => {
                let before = output.len() / 262144;
                output.extend_from_slice(&buffer[..n]);
                remaining -= n;
                if output.len() / 262144 > before {
                    info!(bytes = output.len(), "receiving upload body");
                }
                if probe.as_mut().is_some_and(|p| p.complete(output)) {
                    info!(bytes = output.len(), "complete upload archive received; finishing immediately");
                    return Ok(false);
                }
            }
            Err(e) if upload_stream_interrupted(&e) => return Ok(false),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(true)
}

#[cfg(test)]
mod upload_stream_tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn finishes_complete_archive_before_chunk_ending() {
        let archive = crate::cpio::build_odc_cpio(&[("photo.txt", b"whole file")]);
        let dvzip = crate::dvzip::cpio_to_dvzip(&archive);
        let mut wire = format!("{:x}\r\n", dvzip.len()).into_bytes();
        wire.extend_from_slice(&dvzip);
        let stop_at = wire.len();
        wire.extend_from_slice(b"\r\n0\r\n\r\nNOT_READ");
        let mut reader = Cursor::new(wire);
        let (body, ended) = read_upload_stream(&mut reader,
            "Transfer-Encoding: chunked\r\nContent-Type: application/x-dvzip\r\n").unwrap();
        assert!(!ended);
        assert_eq!(reader.position() as usize, stop_at);
        assert_eq!(body, dvzip);
    }

    #[test]
    fn probe_waits_for_all_blocks_and_rejects_truncated_files() {
        let archive = crate::cpio::build_odc_cpio(&[("photo.txt", b"whole file")]);
        let dvzip = crate::dvzip::cpio_to_dvzip_blocks(&archive, 3);
        let mut probe = UploadArchiveProbe::new("Content-Type: application/x-dvzip\r\n");
        for n in 0..dvzip.len() { assert!(!probe.complete(&dvzip[..n])); }
        assert!(probe.complete(&dvzip));
        assert_eq!(probe.archive, archive);
    }

    #[test]
    fn missing_http_end_preserves_only_verifiable_archive() {
        let archive = crate::cpio::build_odc_cpio(&[("photo.txt", b"whole file")]);
        let mut wire = format!("{:x}\r\n", archive.len()).into_bytes();
        wire.extend_from_slice(&archive);
        wire.extend_from_slice(b"\r\n");
        let (body, ended) = read_upload_stream(&mut Cursor::new(wire), "Transfer-Encoding: chunked\r\n").unwrap();
        assert!(!ended);
        assert_eq!(body, archive);
        assert!(crate::cpio::odc_archive_complete(&body));
        let short = &body[..body.len() / 2];
        assert!(!crate::cpio::odc_archive_complete(short));
    }

    #[test]
    fn stored_dvzip_and_compressed_blocks_verify_identically() {
        let archive = crate::cpio::build_odc_cpio(&[("photo.txt", b"whole file")]);
        let mut stored = ((archive.len() as u32) | 0x80000000).to_be_bytes().to_vec();
        stored.extend_from_slice(&archive);
        let compressed = crate::dvzip::cpio_to_dvzip(&archive);
        for bytes in [stored.as_slice(), compressed.as_slice()] {
            let decoded = crate::dvzip::dvzip_to_cpio_checked(bytes).unwrap();
            assert_eq!(decoded, archive);
            assert!(crate::cpio::odc_archive_complete(&decoded));
            assert!(crate::dvzip::dvzip_to_cpio_checked(&bytes[..bytes.len() - 8]).is_err());
        }
    }

    #[test]
    fn chunked_upload_reads_all_trailers_and_leaves_next_request() {
        let mut reader = Cursor::new(b"3\r\nabc\r\n0\r\nX-Trailer: 1\r\nAnother: 2\r\n\r\nNEXT");
        let (body, ended) = read_upload_stream(&mut reader, "Transfer-Encoding: chunked\r\n").unwrap();
        assert!(ended);
        assert_eq!(body, b"abc");
        assert_eq!(&reader.get_ref()[reader.position() as usize..], b"NEXT");
    }
}

fn read_chunked_stream(reader: &mut impl BufRead) -> Result<Vec<u8>> {
    let mut raw = Vec::new();
    loop {
        let mut size_line = String::new();
        let n = reader.read_line(&mut size_line)?;
        if n == 0 {
            anyhow::bail!("chunked body: EOF before last-chunk (truncated)");
        }
        raw.extend_from_slice(size_line.as_bytes());
        // Strip any chunk-extension (";name=value"); only the leading hex
        // token is the size.
        let size_token = size_line.trim().split(';').next().unwrap_or("").trim();
        let size = match usize::from_str_radix(size_token, 16) {
            Ok(s) => s,
            Err(_) => {
                // Distinguish the genuine last-chunk ("0") from a parse
                // FAILURE. A real "0" parses fine; anything else that fails
                // to parse as hex is a CRLF desync / partial read, NOT a
                // last-chunk. Bail loudly rather than silently truncating.
                anyhow::bail!(
                    "chunked body: malformed chunk-size line {:?} \
                     (not valid hex; would have silently truncated with unwrap_or(0))",
                    size_line.trim()
                );
            }
        };
        if size == 0 {
            // Genuine last-chunk: consume the trailing CRLF (and any
            // optional trailer-part up to the blank line).
            let mut trailer = String::new();
            reader.read_line(&mut trailer)?;
            raw.extend_from_slice(trailer.as_bytes());
            break;
        }
        let mut data = vec![0u8; size];
        reader.read_exact(&mut data)?;
        raw.extend_from_slice(&data);
        let mut crlf = [0u8; 2];
        reader.read_exact(&mut crlf)?;
        raw.extend_from_slice(&crlf);
    }
    Ok(raw)
}

// ---- raw /Upload body capture (LUFTLIFT_DUMP_UPLOAD) ----

/// Read the `LUFTLIFT_DUMP_UPLOAD` env var as a directory path. Returns None
/// if unset (the default — capture is off) or if the path doesn't exist (we
/// don't auto-create it to avoid surprising filesystem writes).
pub fn dump_upload_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("LUFTLIFT_DUMP_UPLOAD")?;
    let path = PathBuf::from(dir);
    if path.is_dir() {
        Some(path)
    } else {
        warn!(
            dir = %path.display(),
            "LUFTLIFT_DUMP_UPLOAD is set but is not a directory; raw upload capture disabled"
        );
        None
    }
}

/// Sanitize a sender name for use in a filename: keep alphanumerics, dash,
/// underscore, dot; replace everything else (spaces, apostrophes in
/// "Andrew's MacBook Pro", etc.) with `_`. Pure (no I/O) so it's unit-testable.
pub fn sanitize_sender_for_filename(sender: &str) -> String {
    sender
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Write a raw /Upload body to `<dir>/upload-<unix_ts>-<sanitized_sender>.bin`
/// for offline decode diffing against the Python reference (`dvzip.py`).
/// Best-effort: logs a warning on failure but never propagates the error —
/// the dump is a debugging aid, not part of the receive path.
pub fn dump_raw_upload(dir: &std::path::Path, sender: &str, body: &[u8]) {
    dump_raw_body(dir, "upload", sender, body);
}

/// Dump a raw request body to `dir` as `<kind>-<ts>-<sender>.bin`. Used for the
/// env-gated capture of /Upload and /Ask bodies (LUFTLIFT_DUMP_UPLOAD).
pub fn dump_raw_body(dir: &std::path::Path, kind: &str, sender: &str, body: &[u8]) {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let sender_part = if sender.is_empty() {
        "unknown".to_string()
    } else {
        sanitize_sender_for_filename(sender)
    };
    let filename = format!("{kind}-{ts}-{sender_part}.bin");
    let path = dir.join(&filename);
    match std::fs::write(&path, body) {
        Ok(_) => info!(
            path = %path.display(),
            bytes = body.len(),
            kind,
            "dumped raw request body (LUFTLIFT_DUMP_UPLOAD)"
        ),
        Err(e) => warn!(path = %path.display(), error = %e, kind, "failed to dump raw request body"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_save_flattens_paths_and_preserves_duplicates() {
        let dir = std::env::temp_dir().join(format!("airdrop-save-{}-{}", std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        let first = save_received_file(&dir, "../../photo.jpg", b"first").unwrap();
        let second = save_received_file(&dir, "/elsewhere/photo.jpg", b"second").unwrap();
        assert_eq!(first, dir.join("photo.jpg"));
        assert_eq!(second, dir.join("photo (1).jpg"));
        assert_eq!(std::fs::read(first).unwrap(), b"first");
        assert_eq!(std::fs::read(second).unwrap(), b"second");
        assert!(save_received_file(&dir, "..", b"bad").is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
    use std::io::Cursor;

    fn cfg() -> ReceiverConfig {
        ReceiverConfig {
            computer_name: "luftlift-pc".into(),
            computer_model: "MacBookPro".into(),
            record_data: None,
        }
    }

    fn find_crlf_crlf(buf: &[u8]) -> usize {
        buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap()
    }

    // ---- introspect counter wiring ----

    fn req_for(path: &str, body: Vec<u8>) -> HttpRequest {
        use std::collections::HashMap;
        HttpRequest {
            method: "POST".into(),
            path: path.into(),
            headers: HashMap::new(),
            body,
        }
    }

    fn ask_body(sender: &str) -> Vec<u8> {
        // Build a minimal binary-plist Ask body with just the sender name.
        use plist::{Dictionary, Value};
        let mut d = Dictionary::new();
        d.insert("TransferID".into(), Value::String("X".into()));
        d.insert("TransferType".into(), Value::String("links".into()));
        d.insert("SenderComputerName".into(), Value::String(sender.into()));
        d.insert("SenderModelName".into(), Value::String("iPhone".into()));
        d.insert("BundleID".into(), Value::String("com.apple.sharing".into()));
        let mut body = Vec::new();
        Value::Dictionary(d).to_writer_binary(&mut body).unwrap();
        body
    }

    #[test]
    fn bump_introspect_for_request_dispatches_by_path() {
        let st = std::sync::Arc::new(IntrospectState::new());
        bump_introspect_for_request(&st, &req_for("/Discover", Vec::new()));
        bump_introspect_for_request(&st, &req_for("/Discover", Vec::new()));
        bump_introspect_for_request(&st, &req_for("/Ask", ask_body("iPhone 15")));
        bump_introspect_for_request(&st, &req_for("/Upload", Vec::new()));
        let s = st.snapshot();
        assert_eq!(s.discover_count, 2);
        assert_eq!(s.ask_count, 1);
        assert_eq!(s.upload_count, 1);
        assert_eq!(s.last_peer, "iPhone 15", "Ask must populate last_peer");
    }

    #[test]
    fn bump_introspect_for_request_unknown_path_does_nothing() {
        let st = std::sync::Arc::new(IntrospectState::new());
        bump_introspect_for_request(&st, &req_for("/Unknown", Vec::new()));
        let s = st.snapshot();
        assert_eq!(s.discover_count, 0);
        assert_eq!(s.ask_count, 0);
        assert_eq!(s.upload_count, 0);
        assert_eq!(s.last_peer, "");
    }

    #[test]
    fn bump_introspect_for_request_ask_with_unparseable_body_still_bumps() {
        // A malformed /Ask body must still bump ask_count — the counter
        // answers "did an Apple sender even reach /Ask", which is more
        // useful for diagnosis than "did we parse the body". The last_peer
        // just isn't updated.
        let st = std::sync::Arc::new(IntrospectState::new());
        bump_introspect_for_request(&st, &req_for("/Ask", b"garbage".to_vec()));
        let s = st.snapshot();
        assert_eq!(s.ask_count, 1);
        assert_eq!(s.last_peer, "");
    }

    #[test]
    fn handle_connection_discover_writes_plist_response() {
        let request = b"POST /Discover HTTP/1.1\r\n\
                        Host: localhost\r\n\
                        Content-Length: 0\r\n\
                        \r\n";
        let mut reader = Cursor::new(&request[..]);
        let mut writer = Vec::new();
        let result = handle_one_connection(&mut reader, &mut writer, &cfg()).unwrap();
        assert_eq!(result.response.status, 200);

        // Find header/body boundary in raw bytes (body is binary plist).
        let sep = find_crlf_crlf(&writer);
        let headers = std::str::from_utf8(&writer[..sep]).unwrap();
        assert!(headers.starts_with("HTTP/1.1 200 OK\r\n"));
        let body = &writer[sep + 4..];
        assert_eq!(&body[..8], b"bplist00");
    }

    #[test]
    fn handle_connection_chunked_ask_body() {
        // iOS-18 style: chunked body, no Content-Length.
        let request = b"POST /Ask HTTP/1.1\r\n\
                        Host: localhost\r\n\
                        Transfer-Encoding: chunked\r\n\
                        \r\n\
                        0\r\n\
                        \r\n";
        let mut reader = Cursor::new(&request[..]);
        let mut writer = Vec::new();
        let result = handle_one_connection(&mut reader, &mut writer, &cfg()).unwrap();
        assert_eq!(result.response.status, 200);
        let sep = find_crlf_crlf(&writer);
        assert_eq!(&writer[sep + 4..sep + 12], b"bplist00");
    }

    #[test]
    fn handle_connection_unknown_path_returns_404() {
        let request = b"POST /Unknown HTTP/1.1\r\nContent-Length: 0\r\n\r\n";
        let mut reader = Cursor::new(&request[..]);
        let mut writer = Vec::new();
        let result = handle_one_connection(&mut reader, &mut writer, &cfg()).unwrap();
        assert_eq!(result.response.status, 404);
        let headers = std::str::from_utf8(&writer).unwrap();
        assert!(headers.starts_with("HTTP/1.1 404"));
    }

    #[test]
    fn handle_connection_upload_dvzip_returns_files() {
        // Build a dvzip payload via the public encoders (one file "test.txt").
        use crate::{cpio::build_odc_cpio, dvzip::cpio_to_dvzip};
        let dvzip = cpio_to_dvzip(&build_odc_cpio(&[("test.txt", b"hello")]));

        let request = format!(
            "POST /Upload HTTP/1.1\r\n\
             Host: localhost\r\n\
             Content-Type: application/x-dvzip\r\n\
             Content-Length: {}\r\n\
             \r\n",
            dvzip.len()
        );
        let mut raw = request.into_bytes();
        raw.extend_from_slice(&dvzip);

        let mut reader = Cursor::new(&raw[..]);
        let mut writer = Vec::new();
        let result = handle_one_connection(&mut reader, &mut writer, &cfg()).unwrap();
        assert_eq!(result.response.status, 200);
        assert_eq!(result.received_files.len(), 1);
        assert_eq!(result.received_files[0].name, "test.txt");
        assert_eq!(result.received_files[0].bytes, b"hello");
    }

    #[test]
    fn handle_connection_sends_100_continue_for_expect() {
        // A sender that sends Expect: 100-continue must receive an interim
        // HTTP/1.1 100 Continue before the final response — otherwise the
        // sender deadlocks waiting for the 100 before shipping the body.
        // Mirrors opendrop-patch/server.py.patched::handle_upload's
        // `send_response(100)` branch.
        use crate::{cpio::build_odc_cpio, dvzip::cpio_to_dvzip};
        let dvzip = cpio_to_dvzip(&build_odc_cpio(&[("note.txt", b"expect me")]));
        let request = format!(
            "POST /Upload HTTP/1.1\r\n\
             Host: localhost\r\n\
             Content-Type: application/x-dvzip\r\n\
             Content-Length: {}\r\n\
             Expect: 100-continue\r\n\
             \r\n",
            dvzip.len()
        );
        let mut raw = request.into_bytes();
        raw.extend_from_slice(&dvzip);

        let mut reader = Cursor::new(&raw[..]);
        let mut writer = Vec::new();
        let result = handle_one_connection(&mut reader, &mut writer, &cfg()).unwrap();
        assert_eq!(result.response.status, 200);
        assert_eq!(result.received_files[0].bytes, b"expect me");

        // The writer must contain the interim 100 Continue BEFORE the final
        // 200 response. The body was still extracted, proving the body read
        // happened after the 100 was emitted.
        let out = String::from_utf8_lossy(&writer);
        let cont_idx = out
            .find("HTTP/1.1 100 Continue")
            .expect("interim 100 Continue must be sent");
        let final_idx = out.find("HTTP/1.1 200").expect("final 200 response must follow");
        assert!(cont_idx < final_idx, "100 Continue must precede the final response");
    }

    #[test]
    fn handle_connection_no_100_continue_without_expect_header() {
        // Without Expect: 100-continue there must be no interim response.
        let request = b"POST /Discover HTTP/1.1\r\n\
                         Host: localhost\r\n\
                         Content-Length: 0\r\n\
                         \r\n";
        let mut reader = Cursor::new(&request[..]);
        let mut writer = Vec::new();
        let _ = handle_one_connection(&mut reader, &mut writer, &cfg()).unwrap();
        let out = String::from_utf8_lossy(&writer);
        assert!(!out.contains("100 Continue"), "no 100 Continue without Expect header");
        assert!(out.starts_with("HTTP/1.1 200"));
    }

    // ---- 100 Continue flush regression tests ----
    //
    // On the TLS path, rustls buffers writes internally. If the 100 Continue
    // is written but not flushed, it stays in the buffer and never reaches
    // the Apple sender — which waits forever for the 100 before sending the
    // body, deadlocking the /Upload. The in-memory Cursor+Vec tests above do
    // NOT catch this because Vec writes are immediate. The tests below verify
    // (a) flush() is called after 100 Continue, and (b) a real socket-pair
    // integration with BufWriter (simulating TLS buffering) completes.

    /// A writer wrapper that records whether flush() was called.
    struct FlushTracker<W: std::io::Write> {
        inner: W,
        flush_count: u32,
    }
    impl<W: std::io::Write> std::io::Write for FlushTracker<W> {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.inner.write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.flush_count += 1;
            self.inner.flush()
        }
    }

    #[test]
    fn read_request_respecting_expect_flushes_100_continue() {
        // After writing 100 Continue, flush() MUST be called — without it,
        // the response stays in the TLS encryption buffer and the Apple
        // sender deadlocks waiting for the 100.
        let request = b"POST /Upload HTTP/1.1\r\n\
                         Expect: 100-continue\r\n\
                         Content-Length: 5\r\n\
                         \r\n\
                         hello";
        let mut reader = Cursor::new(&request[..]);
        let mut writer = FlushTracker { inner: Vec::new(), flush_count: 0 };

        let raw = read_request_respecting_expect(&mut reader, &mut writer).unwrap();

        // 100 Continue was written.
        assert!(
            writer.inner.starts_with(b"HTTP/1.1 100 Continue\r\n\r\n"),
            "100 Continue must be written to the writer"
        );
        // flush() was called at least once after the 100 Continue.
        assert!(
            writer.flush_count > 0,
            "flush() must be called after 100 Continue — without it, the \
             response stays buffered in TLS/BufWriter and the Apple sender \
             deadlocks waiting for it"
        );
        // Body was still read correctly.
        assert!(raw.windows(5).any(|w| w == b"hello"), "body must be present in raw");
    }

    #[test]
    fn handle_upload_chunked_expect_100_continue_full_flow_no_deadlock() {
        // Full Apple-style Upload: chunked + Expect:100-continue, multi-chunk
        // dvzip body, over a real TCP socket pair with a BufWriter on the
        // server side (to simulate TLS write buffering). Without the flush
        // after 100 Continue, the client's read for the 100 times out
        // (deadlock caught by set_read_timeout).
        use std::io::{Read, Write as IoWrite};
        use std::net::TcpListener;
        use std::time::Duration;

        // Build a real dvzip(CPIO) payload with a test file.
        let entries: &[(&str, &[u8])] = &[("expect_test.txt", b"chunked + expect body")];
        let cpio = crate::cpio::build_odc_cpio(entries);
        let dvzip = crate::dvzip::cpio_to_dvzip(&cpio);

        // Frame as chunked, split into 2 chunks to exercise multi-chunk decoding.
        let mid = dvzip.len() / 2;
        let rest = dvzip.len() - mid;
        let mut chunked_body = Vec::new();
        chunked_body.extend_from_slice(format!("{:x}\r\n", mid).as_bytes());
        chunked_body.extend_from_slice(&dvzip[..mid]);
        chunked_body.extend_from_slice(b"\r\n");
        chunked_body.extend_from_slice(format!("{:x}\r\n", rest).as_bytes());
        chunked_body.extend_from_slice(&dvzip[mid..]);
        chunked_body.extend_from_slice(b"\r\n0\r\n\r\n");

        // Bind a local listener for a real TCP socket pair.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");

        // Server in background thread. BufWriter simulates TLS's internal
        // write buffering — if 100 Continue isn't explicitly flushed, it
        // stays in the BufWriter and never reaches the client.
        let server_cfg = cfg();
        let server_thread = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let write_stream = stream.try_clone().expect("try_clone");
            let mut reader = std::io::BufReader::new(stream);
            let mut writer = std::io::BufWriter::new(write_stream);
            handle_one_connection(&mut reader, &mut writer, &server_cfg)
        });

        // Client (Apple sender simulation): send headers with chunked +
        // Expect:100-continue, WAIT for 100 Continue, then send the body.
        let mut stream =
            TcpStream::connect_timeout(&addr, Duration::from_secs(5)).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("set_read_timeout");

        let headers = "POST /Upload HTTP/1.1\r\n\
                       Host: localhost\r\n\
                       Content-Type: application/x-dvzip\r\n\
                       Transfer-Encoding: chunked\r\n\
                       Expect: 100-continue\r\n\
                       \r\n";
        stream.write_all(headers.as_bytes()).expect("write headers");
        stream.flush().expect("flush headers");

        // WAIT for 100 Continue. If the server didn't flush, this read times
        // out after 10s and .expect() panics — catching the regression.
        let mut buf = [0u8; 1024];
        let n = stream
            .read(&mut buf)
            .expect("read 100-continue (timed out = server didn't flush the 100 Continue)");
        let cont = String::from_utf8_lossy(&buf[..n]);
        assert!(cont.contains("100 Continue"), "expected 100 Continue, got: {cont}");

        // 100 Continue received — send the chunked body.
        stream.write_all(&chunked_body).expect("write chunked body");
        stream.flush().expect("flush body");

        // Read the final 200 response.
        let mut resp_buf = Vec::new();
        stream.read_to_end(&mut resp_buf).expect("read final response");
        let resp = String::from_utf8_lossy(&resp_buf);
        assert!(resp.contains("200"), "final response should be 200: {resp}");

        // Verify the server-side result.
        let result = server_thread.join().expect("server thread").expect("handle");
        assert_eq!(result.response.status, 200);
        assert_eq!(result.received_files.len(), 1);
        assert_eq!(result.received_files[0].name, "expect_test.txt");
        assert_eq!(result.received_files[0].bytes, b"chunked + expect body");
    }

    // ---- raw /Upload capture (LUFTLIFT_DUMP_UPLOAD) ----

    #[test]
    fn sanitize_sender_replaces_unsafe_chars() {
        // "Andrew's MacBook Pro" -> "Andrew_s_MacBook_Pro"
        assert_eq!(sanitize_sender_for_filename("Andrew's MacBook Pro"), "Andrew_s_MacBook_Pro");
        // Alphanumeric / -_./ are kept.
        assert_eq!(sanitize_sender_for_filename("iPhone42"), "iPhone42");
        assert_eq!(sanitize_sender_for_filename("luftlift-pc.local"), "luftlift-pc.local");
        assert_eq!(sanitize_sender_for_filename("a_b-c.d"), "a_b-c.d");
    }

    #[test]
    fn sanitize_sender_handles_empty_and_unicode() {
        assert_eq!(sanitize_sender_for_filename(""), "");
        // Non-ASCII chars are replaced with `_`; ASCII alphanumerics survive.
        // Use a string of only non-ASCII chars so output is all underscores.
        let s = sanitize_sender_for_filename("üïöé");
        assert!(s.chars().all(|c| c == '_'), "expected all underscores, got {s:?}");
        // Mixed: ASCII kept, non-ASCII replaced.
        assert_eq!(sanitize_sender_for_filename("café"), "caf_");
    }

    #[test]
    fn dump_raw_upload_writes_file_with_sender_and_timestamp_pattern() {
        // Best-effort dump: writes <dir>/upload-<ts>-<sender>.bin with the
        // raw body bytes. Verify in a tempdir.
        let dir = std::env::temp_dir();
        let body = b"raw dvzip body bytes";
        dump_raw_upload(&dir, "iPhone42", body);
        // Find the most recent matching file and verify its content + name.
        let pattern = "upload-";
        let mut found: Option<PathBuf> = None;
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with(pattern)
                    && name.ends_with("-iPhone42.bin")
                    && entry.metadata().map(|m| m.len() as usize == body.len()).unwrap_or(false)
                {
                    let candidate = entry.path();
                    if found.as_ref().is_none_or(|p| {
                        candidate.metadata().and_then(|m| m.modified()).ok()
                            > p.metadata().and_then(|m| m.modified()).ok()
                    }) {
                        found = Some(candidate);
                    }
                }
            }
        }
        let path = found.expect("dump file must exist after dump_raw_upload");
        let read_back = std::fs::read(&path).expect("read dumped file");
        assert_eq!(read_back, body, "dumped bytes must match the input body");
        let fname = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(fname.contains("-iPhone42.bin"), "filename must include sender: {fname}");
        assert!(fname.starts_with("upload-"), "filename must start with upload-: {fname}");
        // Cleanup.
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn dump_raw_upload_uses_unknown_for_empty_sender() {
        let dir = std::env::temp_dir();
        let body = b"x";
        dump_raw_upload(&dir, "", body);
        // Find a matching upload-*-unknown.bin from the last few seconds.
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with("upload-") && name.ends_with("-unknown.bin") {
                    let _ = std::fs::remove_file(entry.path());
                    return; // pass
                }
            }
        }
        panic!("expected an upload-*-unknown.bin file after dumping with empty sender");
    }
}
