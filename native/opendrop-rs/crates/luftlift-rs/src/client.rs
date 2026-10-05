//! AirDrop **sender** (client) — builds the `/Discover`, `/Ask`, `/Upload`
//! requests as pure functions and orchestrates the POST sequence over a TLS
//! connection. The network glue is kept behind a small trait so the request
//! building + response handling is unit-testable without sockets.
//!
//! Reference: `docs/airdrop-protocol.md` (HTTP flow + iOS-18 quirks) and
//! `opendrop-patch/server.py.patched` (the receiver these requests target).

use crate::dvzip::cpio_to_dvzip;
use crate::cpio::build_odc_cpio;
use crate::http::{HttpRequest, HttpResponse};
use crate::plist_impl::ReceiverConfig;
use anyhow::Result;
use plist::{Dictionary, Value};
use std::collections::HashMap;
use tracing::info;

/// Frame a payload as an HTTP/1.1 chunked body: a single chunk of `body`
/// followed by the terminating `0\r\n\r\n`. Used to re-frame the /Upload body
/// as chunked to mimic a real Apple sender.
fn chunk_frame(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 16);
    if !body.is_empty() {
        out.extend_from_slice(format!("{:x}\r\n", body.len()).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"0\r\n\r\n");
    out
}

/// Sender identity/configuration used to build /Discover and /Ask requests.
#[derive(Debug, Clone)]
pub struct SenderConfig {
    pub computer_name: String,
    pub model_name: String,
    pub bundle_id: String,
    /// Random six-byte sender identity, encoded as a hexadecimal string in
    /// the /Ask request for anonymous Everyone-mode transfers.
    pub sender_id: Vec<u8>,
}

impl SenderConfig {
    /// Build a `SenderConfig` with a freshly generated 6-byte `SenderID`.
    /// The value is random-but-stable-for-the-process so multiple transfers
    /// from one `luftlift send` invocation share a sender identity.
    pub fn new(computer_name: impl Into<String>, model_name: impl Into<String>, bundle_id: impl Into<String>) -> Self {
        Self {
            computer_name: computer_name.into(),
            model_name: model_name.into(),
            bundle_id: bundle_id.into(),
            sender_id: random_sender_id(),
        }
    }

    /// Convenience for tests: explicit sender_id bytes.
    #[cfg(test)]
    pub fn with_sender_id(
        computer_name: impl Into<String>,
        model_name: impl Into<String>,
        bundle_id: impl Into<String>,
        sender_id: Vec<u8>,
    ) -> Self {
        Self {
            computer_name: computer_name.into(),
            model_name: model_name.into(),
            bundle_id: bundle_id.into(),
            sender_id,
        }
    }
}

/// Generate a 6-byte pseudo-random sender ID (MAC-address-sized, matching the
/// shape real Apple senders use). Uses the same time-seeded PRNG as the
/// transfer-id generator in main.rs — not cryptographic, but adequate for an
/// AirDrop sender identifier that only needs to be unique per process.
fn random_sender_id() -> Vec<u8> {
    use std::time::SystemTime;
    let s = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let seed = s.as_nanos() as u64 ^ (std::process::id() as u64).rotate_left(17);
    // xorshift64* → 6 bytes. Good enough dispersion for a non-crypto ID.
    let mut x = seed.wrapping_mul(0x2545F4914F6CDD1D).max(1);
    let mut out = [0u8; 6];
    for byte in &mut out {
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        *byte = (x >> 24) as u8;
    }
    out.to_vec()
}

/// One file to send.
#[derive(Debug, Clone)]
pub struct FileToSend {
    pub name: String,
    /// UTI type, e.g. "public.jpeg" for JPEGs, "com.apple.web-internet-location"
    /// for links. opendrop uses "public.data" as a generic fallback.
    pub uti_type: String,
    pub data: Vec<u8>,
}

/// Build the `/Discover` request body (sender record plist). The receiver
/// responds with its own record so we appear in its picker — actually for a
/// sender, /Discover makes the *receiver* appear in *our* picker. Body is a
/// minimal sender plist; Content-Type application/x-apple-plist.
pub fn build_discover_request(cfg: &SenderConfig) -> HttpRequest {
    let mut dict = Dictionary::new();
    dict.insert("SenderComputerName".into(), Value::String(cfg.computer_name.clone()));
    dict.insert("SenderModelName".into(), Value::String(cfg.model_name.clone()));
    let mut body = Vec::new();
    Value::Dictionary(dict).to_writer_binary(&mut body).expect("serialise discover request");

    let mut headers = HashMap::new();
    headers.insert("host".into(), "airdrop._tcp.local.".into());
    headers.insert("content-type".into(), "application/x-apple-plist".into());
    HttpRequest {
        method: "POST".into(),
        path: "/Discover".into(),
        headers,
        body,
    }
}

/// Build the `/Ask` request body (file metadata plist). `transfer_id` is a
/// caller-generated UUID; the receiver auto-accepts by replying 200.
pub fn build_ask_request(cfg: &SenderConfig, transfer_id: &str, files: &[FileToSend]) -> HttpRequest {
    let mut files_arr = Vec::new();
    for f in files {
        let mut entry = Dictionary::new();
        entry.insert("FileName".into(), Value::String(f.name.clone()));
        entry.insert("FileType".into(), Value::String(f.uti_type.clone()));
        entry.insert("FileSize".into(), Value::Integer((f.data.len() as i64).into()));
        entry.insert("FileBomPath".into(), Value::String(format!("./{}", f.name)));
        entry.insert("FileIsDirectory".into(), Value::Boolean(false));
        entry.insert("ConvertMediaFormats".into(), Value::Integer(0.into()));
        files_arr.push(Value::Dictionary(entry));
    }

    let mut root = Dictionary::new();
    let mut identifier = Dictionary::new();
    identifier.insert("id".into(), Value::String(transfer_id.into()));
    root.insert("TransferID".into(), Value::Dictionary(identifier));
    let mut kind = Dictionary::new();
    kind.insert("files".into(), Value::Dictionary(Dictionary::new()));
    root.insert("TransferType".into(), Value::Dictionary(kind));
    // Apple receivers expect the hexadecimal service identifier as a string,
    // matching OpenDrop's client, rather than the binary contact-record form.
    let sender_id: String = cfg.sender_id.iter().map(|byte| format!("{byte:02x}")).collect();
    root.insert("SenderID".into(), Value::String(sender_id));
    root.insert("SenderComputerName".into(), Value::String(cfg.computer_name.clone()));
    root.insert("SenderModelName".into(), Value::String(cfg.model_name.clone()));
    root.insert("BundleID".into(), Value::String(cfg.bundle_id.clone()));
    root.insert("Files".into(), Value::Array(files_arr));
    root.insert("Items".into(), Value::Array(Vec::new()));
    root.insert("ConvertMediaFormats".into(), Value::Boolean(false));

    let mut body = Vec::new();
    Value::Dictionary(root).to_writer_binary(&mut body).expect("serialise ask request");

    let mut headers = HashMap::new();
    headers.insert("host".into(), "airdrop._tcp.local.".into());
    headers.insert("content-type".into(), "application/x-apple-plist".into());
    HttpRequest {
        method: "POST".into(),
        path: "/Ask".into(),
        headers,
        body,
    }
}

/// Build the `/Upload` request body: a CPIO archive of the files, framed as
/// dvzip (iOS-18 Content-Type `application/x-dvzip`).
pub fn build_upload_request(files: &[FileToSend]) -> HttpRequest {
    let entries: Vec<(&str, &[u8])> =
        files.iter().map(|f| (f.name.as_str(), f.data.as_slice())).collect();
    let cpio = build_odc_cpio(&entries);
    let dvzip = cpio_to_dvzip(&cpio);

    let mut headers = HashMap::new();
    headers.insert("host".into(), "airdrop._tcp.local.".into());
    headers.insert("content-type".into(), "application/x-dvzip".into());
    HttpRequest {
        method: "POST".into(),
        path: "/Upload".into(),
        headers,
        body: dvzip,
    }
}

/// Verify the receiver's /Discover response carries the expected receiver
/// record fields. Returns the parsed ReceiverComputerName on success.
pub fn parse_discover_response(resp: &HttpResponse) -> Result<ReceiverConfig> {
    if resp.status != 200 {
        anyhow::bail!("/Discover returned status {}", resp.status);
    }
    let val = Value::from_reader(std::io::Cursor::new(&resp.body))?;
    let dict = val.as_dictionary().ok_or_else(|| anyhow::anyhow!("/Discover response not a dict"))?;
    Ok(ReceiverConfig {
        computer_name: dict
            .get("ReceiverComputerName")
            .and_then(|v| v.as_string())
            .unwrap_or("unknown")
            .to_string(),
        computer_model: dict
            .get("ReceiverModelName")
            .and_then(|v| v.as_string())
            .unwrap_or("unknown")
            .to_string(),
        record_data: dict.get("ReceiverRecordData").and_then(|v| v.as_data()).map(|d| d.to_vec()),
    })
}

// ---- thin network trait ----

/// A minimal HTTP-over-TLS transport. Implementations talk to a real
/// `rustls::StreamOwned<TcpStream>`; the in-memory test impl round-trips
/// through the crate's own server handler.
pub trait AirDropTransport {
    /// Send a request and read back the response.
    fn send(&mut self, req: &HttpRequest) -> Result<HttpResponse>;
}

/// Run the full AirDrop send flow against a peer over the given transport.
/// Sequence: POST /Discover → POST /Ask → POST /Upload.
pub fn send_files(
    transport: &mut impl AirDropTransport,
    sender: &SenderConfig,
    files: &[FileToSend],
    transfer_id: &str,
) -> Result<()> {
    info!("POST /Discover");
    let req = build_discover_request(sender);
    let resp = transport.send(&req)?;
    let receiver = parse_discover_response(&resp)?;
    info!(receiver = %receiver.computer_name, "receiver appeared");

    info!(transfer_id, "POST /Ask");
    let req = build_ask_request(sender, transfer_id, files);
    let resp = transport.send(&req)?;
    if resp.status != 200 {
        anyhow::bail!("/Ask rejected (status {})", resp.status);
    }
    info!("receiver accepted transfer");

    info!("POST /Upload ({} files)", files.len());
    let mut req = build_upload_request(files);
    req.headers.insert("transferid".into(), transfer_id.into());
    req.headers.insert("totalbytes".into(), req.body.len().to_string());
    // Send chunked data immediately, matching the verified iOS upload flow.
    req.headers.insert("transfer-encoding".into(), "chunked".into());
    // iOS binds Upload to the identifier it accepted in Ask.
    req.body = chunk_frame(&req.body);
    let resp = transport.send(&req)?;
    if resp.status != 200 {
        anyhow::bail!("/Upload failed (status {})", resp.status);
    }
    let total: usize = files.iter().map(|f| f.data.len()).sum();
    info!(files = files.len(), bytes = total, "upload complete");
    Ok(())
}

// ---- real TLS transport ----

use crate::http::{format_request, parse_response};
use rustls::ClientConfig;
use std::io::BufReader;
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;

/// A real HTTP-over-TLS transport targeting a single AirDrop receiver.
/// Discovery uses a separate connection; Ask and Upload share one connection
/// so the receiver retains its approval state.
pub struct RustlsTransport {
    config: Arc<ClientConfig>,
    peer: SocketAddr,
    connection: Option<BufReader<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>>,
    timeout: std::time::Duration,
}

impl RustlsTransport {
    pub fn new(config: Arc<ClientConfig>, peer: SocketAddr) -> Self {
        Self { config, peer, connection: None, timeout: std::time::Duration::from_secs(120) }
    }

    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

impl AirDropTransport for RustlsTransport {
    fn send(&mut self, req: &HttpRequest) -> Result<HttpResponse> {
        // Discovery uses a separate connection. Ask and Upload must share
        // their connection, matching the Apple receiver's approval state.
        if req.path == "/Ask" { self.connection = None; }
        if self.connection.is_none() {
            let tcp = TcpStream::connect_timeout(&self.peer, self.timeout.min(std::time::Duration::from_secs(15)))?;
            tcp.set_read_timeout(Some(self.timeout))?;
            tcp.set_write_timeout(Some(self.timeout))?;
            let server_name = rustls::pki_types::ServerName::try_from("airdrop")?;
            let conn = rustls::ClientConnection::new(self.config.clone(), server_name)?;
            self.connection = Some(BufReader::new(rustls::StreamOwned::new(conn, tcp)));
        }
        let reader = self.connection.as_mut().unwrap();
        let mut request = req.clone();
        request.headers.insert("connection".into(), "keep-alive".into());
        request.headers.insert("user-agent".into(), "AirDrop/1.0".into());
        if matches!(request.path.as_str(), "/Discover" | "/Ask") {
            request.headers.insert("content-type".into(), "application/octet-stream".into());
        }
        let wire = format_request(&request);
        use std::io::Write as _;
        reader.get_mut().write_all(&wire)?;
        reader.get_mut().flush()?;
        let raw = read_full_response(reader)?;
        let resp = parse_response(&raw)?;
        info!(path = %req.path, status = resp.status, bytes = resp.body.len(), "AirDrop response received");
        if resp.headers.iter().any(|(key, value)| key.eq_ignore_ascii_case("connection") && value.eq_ignore_ascii_case("close")) {
            self.connection = None;
        }
        Ok(resp)
    }
}

/// Read a full HTTP response (status line + headers + Content-Length body)
/// from a BufRead. Mirrors `server::read_full_request` but for responses.
///
/// Handles interim `1xx` responses per RFC 7231 §6.2: a client MUST be able
/// to parse one or more 1xx responses received prior to a final response,
/// even if it did not expect a 100-Continue. We skip any 1xx block and
/// continue reading until the final (non-1xx) response arrives.
fn read_full_response(reader: &mut impl std::io::BufRead) -> Result<Vec<u8>> {
    loop {
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
            anyhow::bail!("connection closed before response headers complete");
        }
        // If this is an interim 1xx response (100 Continue, 102 Processing,
        // …), discard it and read the next response block. RFC 7231 §6.2:
        // "a client MUST be able to parse one or more 1xx responses received
        // prior to a final response, even if the client does not expect a
        // 100-Continue status message."
        let head = std::str::from_utf8(&buf).unwrap_or("");
        let status_line = head.lines().next().unwrap_or("");
        let is_interim = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<u16>().ok())
            .map(|status| (100..200).contains(&status))
            .unwrap_or(false);
        if is_interim {
            continue;
        }
        // iPhone receivers may return chunked binary plists.
        if parse_header_value(head, "transfer-encoding").map(|v| v.to_ascii_lowercase().contains("chunked")).unwrap_or(false) {
            loop {
                let mut line = String::new();
                anyhow::ensure!(reader.read_line(&mut line)? > 0, "truncated response chunk size");
                let length = usize::from_str_radix(line.trim().split(';').next().unwrap_or(""), 16)?;
                anyhow::ensure!(length <= 8 * 1024 * 1024 && buf.len() + length <= 8 * 1024 * 1024, "AirDrop response too large");
                buf.extend_from_slice(line.as_bytes());
                if length == 0 {
                    loop {
                        line.clear();
                        anyhow::ensure!(reader.read_line(&mut line)? > 0, "truncated response trailers");
                        buf.extend_from_slice(line.as_bytes());
                        if line == "\r\n" { break; }
                    }
                    break;
                }
                let mut block = vec![0; length + 2];
                reader.read_exact(&mut block)?;
                anyhow::ensure!(block.ends_with(b"\r\n"), "invalid response chunk ending");
                buf.extend_from_slice(&block);
            }
        } else if let Some(cl) = parse_header_value(head, "content-length") {
            let len: usize = cl.parse().unwrap_or(0);
            let mut body = vec![0u8; len];
            reader.read_exact(&mut body)?;
            buf.extend_from_slice(&body);
        }
        return Ok(buf);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunked::decode_chunked;
    use crate::dvzip::dvzip_to_cpio;
    use crate::cpio::read_odc_cpio;
    use crate::http::{format_request, parse_request};

    #[test]
    fn chunked_response_with_trailers_leaves_next_response_unread() {
        let wire = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3;test=1\r\nabc\r\n2\r\nde\r\n0\r\nX-Test: yes\r\n\r\nNEXT";
        let mut reader = std::io::Cursor::new(wire);
        let response = parse_response(&read_full_response(&mut reader).unwrap()).unwrap();
        assert_eq!(response.body, b"abcde");
        assert_eq!(&wire[reader.position() as usize..], b"NEXT");
        let mut broken = std::io::Cursor::new(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n8\r\nshort");
        assert!(read_full_response(&mut broken).is_err());
    }
    use crate::handlers::handle_post;
    use crate::plist_impl::ReceiverConfig;

    fn sender() -> SenderConfig {
        SenderConfig::with_sender_id(
            "luftlift-sender",
            "MacBookPro",
            "com.luftlift.app",
            vec![0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE],
        )
    }

    // ---- build_discover_request ----

    #[test]
    fn build_discover_request_is_binary_plist_post() {
        let req = build_discover_request(&sender());
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/Discover");
        assert_eq!(req.header("content-type"), Some("application/x-apple-plist"));
        assert_eq!(&req.body[..8], b"bplist00");
        // The receiver handler should accept it and produce a response.
        let resp = handle_post(&req, &rcfg());
        assert_eq!(resp.response.status, 200);
    }

    // ---- build_ask_request ----

    #[test]
    fn build_ask_request_describes_files() {
        let files = vec![FileToSend {
            name: "photo.jpg".into(),
            uti_type: "public.jpeg".into(),
            data: vec![0xFF, 0xD8, 0xFF],
        }];
        let req = build_ask_request(&sender(), "TID-1", &files);
        assert_eq!(req.path, "/Ask");
        // Receiver should be able to parse our Ask and accept.
        let resp = handle_post(&req, &rcfg());
        assert_eq!(resp.response.status, 200);
    }

    #[test]
    fn build_ask_request_includes_hexadecimal_sender_id() {
        // Modern Apple receivers expect SenderID to be a hexadecimal string.
        let files = vec![FileToSend {
            name: "note.txt".into(),
            uti_type: "public.data".into(),
            data: b"hello".to_vec(),
        }];
        let req = build_ask_request(&sender(), "TID-SID", &files);
        let val = Value::from_reader(std::io::Cursor::new(&req.body)).unwrap();
        let dict = val.as_dictionary().unwrap();
        let sid = dict.get("SenderID").expect("SenderID must be present in Ask body");
        let sid_text = sid.as_string().expect("SenderID must be a string value");
        assert_eq!(sid_text, "deadbeefcafe");
    }

    #[test]
    fn sender_config_new_generates_non_empty_sender_id() {
        let cfg = SenderConfig::new("n", "m", "b");
        assert!(!cfg.sender_id.is_empty(), "default SenderID must not be empty");
        // Two constructions in the same process should be distinct (time-seeded).
        let other = SenderConfig::new("n", "m", "b");
        assert_ne!(cfg.sender_id, other.sender_id, "SenderIDs should differ");
    }

    // ---- build_upload_request ----

    #[test]
    fn build_upload_request_roundtrips_through_receiver() {
        let files = vec![
            FileToSend {
                name: "hello.txt".into(),
                uti_type: "public.data".into(),
                data: b"hi from luftlift".to_vec(),
            },
            FileToSend {
                name: "sweep.bin".into(),
                uti_type: "public.data".into(),
                data: (0u8..=255).collect(),
            },
        ];
        let req = build_upload_request(&files);
        assert_eq!(req.header("content-type"), Some("application/x-dvzip"));
        assert_eq!(req.path, "/Upload");

        // The receiver handler should decode it and extract the files.
        let resp = handle_post(&req, &rcfg());
        assert_eq!(resp.response.status, 200);
        assert_eq!(resp.received_files.len(), 2);
        assert_eq!(resp.received_files[0].name, "hello.txt");
        assert_eq!(resp.received_files[0].bytes, b"hi from luftlift");
        assert_eq!(resp.received_files[1].name, "sweep.bin");
        assert_eq!(resp.received_files[1].bytes, (0u8..=255).collect::<Vec<u8>>());
    }

    #[test]
    fn build_upload_request_body_is_valid_dvzip() {
        let files = vec![FileToSend {
            name: "x".into(),
            uti_type: "public.data".into(),
            data: b"abc".to_vec(),
        }];
        let req = build_upload_request(&files);
        // Directly decode the dvzip body → CPIO → entries.
        let cpio = dvzip_to_cpio(&req.body);
        assert_eq!(&cpio[..6], b"070707");
        let entries = read_odc_cpio(&cpio);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "x");
        assert_eq!(entries[0].data, b"abc");
    }

    // ---- parse_discover_response ----

    #[test]
    fn parse_discover_response_extracts_receiver_name() {
        let resp = handle_post(&build_discover_request(&sender()), &rcfg()).response;
        let recv = parse_discover_response(&resp).unwrap();
        assert_eq!(recv.computer_name, "luftlift-pc");
        assert_eq!(recv.computer_model, "MacBookPro");
    }

    #[test]
    fn parse_discover_response_errors_on_non_200() {
        let resp = HttpResponse { status: 404, body: Vec::new(), headers: Vec::new() };
        assert!(parse_discover_response(&resp).is_err());
    }

    // ---- end-to-end send via in-memory transport ----

    /// An in-memory transport that routes requests through our own server
    /// handler — proving the client requests and server handler interoperate.
    struct InMemoryTransport {
        receiver: ReceiverConfig,
    }
    impl AirDropTransport for InMemoryTransport {
        fn send(&mut self, req: &HttpRequest) -> Result<HttpResponse> {
            // Serialise the request, then re-parse to exercise the wire format.
            let wire = format_request(req);
            let parsed = parse_request(&wire).unwrap();
            let result = handle_post(&parsed, &self.receiver);
            Ok(result.response)
        }
    }

    fn rcfg() -> ReceiverConfig {
        ReceiverConfig {
            computer_name: "luftlift-pc".into(),
            computer_model: "MacBookPro".into(),
            record_data: None,
        }
    }

    #[test]
    fn send_files_end_to_end_succeeds_via_in_memory_transport() {
        let mut transport = InMemoryTransport { receiver: rcfg() };
        let files = vec![FileToSend {
            name: "note.txt".into(),
            uti_type: "public.data".into(),
            data: b"hello world".to_vec(),
        }];
        send_files(&mut transport, &sender(), &files, "TID-E2E").unwrap();
    }

    #[test]
    fn send_files_handles_chunked_request_bodies() {
        // Verify our requests use Content-Length by default (the body is a
        // binary plist, so inspect only the header section of the wire bytes).
        let req = build_ask_request(&sender(), "TID-C", &[]);
        let wire = format_request(&req);
        let header_end = wire.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let headers = std::str::from_utf8(&wire[..header_end]).unwrap();
        assert!(headers.contains("Content-Length"));
        // And our chunked decoder handles a hand-framed body (iOS-18 path).
        let chunked = b"3\r\nabc\r\n0\r\n\r\n";
        assert_eq!(decode_chunked(chunked).unwrap(), b"abc");
    }

    // ---- read_full_response: interim 1xx handling ----

    #[test]
    fn read_full_response_skips_interim_100_continue() {
        // A receiver that sends 100 Continue before the final 200 (RFC 7231
        // §6.2: client MUST parse one or more 1xx responses before the final).
        let raw = b"HTTP/1.1 100 Continue\r\n\r\n\
                    HTTP/1.1 200 OK\r\n\
                    Content-Type: application/x-apple-plist\r\n\
                    Content-Length: 5\r\n\
                    \r\n\
                    hello";
        let mut reader = std::io::Cursor::new(&raw[..]);
        let bytes = read_full_response(&mut reader).unwrap();
        let resp = crate::http::parse_response(&bytes).unwrap();
        assert_eq!(resp.status, 200, "must skip the 100 Continue and return the final 200");
        assert_eq!(resp.body, b"hello");
    }

    #[test]
    fn read_full_response_skips_multiple_1xx_responses() {
        // RFC 7231 allows multiple 1xx responses before the final one.
        let raw = b"HTTP/1.1 100 Continue\r\n\r\n\
                    HTTP/1.1 102 Processing\r\n\r\n\
                    HTTP/1.1 200 OK\r\n\
                    Content-Length: 2\r\n\
                    \r\n\
                    ok";
        let mut reader = std::io::Cursor::new(&raw[..]);
        let bytes = read_full_response(&mut reader).unwrap();
        let resp = crate::http::parse_response(&bytes).unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, b"ok");
    }

    #[test]
    fn read_full_response_no_interim_returns_directly() {
        // No 1xx: just the final response, as before.
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabc";
        let mut reader = std::io::Cursor::new(&raw[..]);
        let bytes = read_full_response(&mut reader).unwrap();
        let resp = crate::http::parse_response(&bytes).unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, b"abc");
    }

    // ---- chunk_frame + Apple-style Upload ----

    #[test]
    fn chunk_frame_single_chunk_roundtrips_through_decode_chunked() {
        let body = b"hello world";
        let framed = chunk_frame(body);
        // Must be: b\r\nhello world\r\n0\r\n\r\n
        assert_eq!(decode_chunked(&framed).unwrap(), body);
    }

    #[test]
    fn chunk_frame_empty_body_yields_just_terminator() {
        let framed = chunk_frame(b"");
        assert_eq!(framed, b"0\r\n\r\n");
        assert_eq!(decode_chunked(&framed).unwrap(), b"");
    }

    #[test]
    fn send_files_upload_uses_chunked_without_expect_100_continue() {
        // Verify immediate chunked upload and a valid archive on the wire.
        struct WireCapturingTransport;
        impl AirDropTransport for WireCapturingTransport {
            fn send(&mut self, req: &HttpRequest) -> Result<HttpResponse> {
                // For /Upload, verify the headers are set correctly.
                if req.path == "/Upload" {
                    assert_eq!(
                        req.header("transfer-encoding"),
                        Some("chunked"),
                        "Upload must use chunked to mimic Apple sender"
                    );
                    assert_eq!(
                        req.header("expect"),
                        None,
                        "Upload must not wait for a 100-continue handshake"
                    );
                    // The body must be chunk-framed (not raw dvzip) — decode it.
                    let decoded =
                        decode_chunked(&req.body).expect("Upload body must be chunked-framed");
                    let cpio = dvzip_to_cpio(&decoded);
                    let entries = read_odc_cpio(&cpio);
                    assert_eq!(entries.len(), 1);
                    assert_eq!(entries[0].name, "f.txt");
                }
                // Return synthetic responses so send_files progresses.
                if req.path == "/Discover" {
                    let cfg = rcfg();
                    Ok(HttpResponse {
                        status: 200,
                        body: crate::plist_impl::build_discover_response(&cfg),
                        headers: Vec::new(),
                    })
                } else {
                    Ok(HttpResponse { status: 200, body: Vec::new(), headers: Vec::new() })
                }
            }
        }

        let mut transport = WireCapturingTransport;
        let files = vec![FileToSend {
            name: "f.txt".into(),
            uti_type: "public.data".into(),
            data: b"data".to_vec(),
        }];
        send_files(&mut transport, &sender(), &files, "TID-X").unwrap();
    }
}
