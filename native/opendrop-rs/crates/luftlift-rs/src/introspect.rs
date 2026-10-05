//! Loopback HTTP/JSON introspection API for the AirDrop receiver.
//!
//! Exposes live receiver state (`/status`), a runtime-toggleable structured
//! trace ring buffer (`GET/POST /trace`), bound to **127.0.0.1 only** so it
//! never reaches the AWDL link. This is SEPARATE from the AirDrop HTTPS
//! listener on 8771 — the two do not share a port or a server.
//!
//! See LUFTLIFT_HTTP_INTROSPECT.md for the spec.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

/// Maximum number of trace events kept in the ring buffer. Older events are
/// dropped on push. The spec calls for ~2000.
pub const TRACE_RING_CAP: usize = 2000;

/// UNIX epoch milliseconds, used as a coarse timestamp origin for
/// `last_announce_ms_ago`. Stored once to avoid re-reading the clock on hot
/// paths.
fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Live, lock-free-ish receiver state for `/status`. Counters are atomic so
/// the AirDrop HTTPS handler thread can bump them without contending with the
/// introspection HTTP thread's snapshot read. The receiver name and last
/// peer are updated rarely, so a `Mutex<String>` is plenty.
pub struct IntrospectState {
    /// `ReceiverComputerName` shown in the Apple AirDrop picker.
    pub receiver_name: Mutex<String>,
    /// True once mDNS announce has succeeded.
    pub mdns_announced: AtomicBool,
    /// Port the AirDrop HTTPS listener is bound to (8771).
    pub https_port: AtomicU16,
    /// ms-since-epoch of the most recent successful mDNS (re-)announce.
    pub last_announce_unix_ms: AtomicU64,
    /// Inbound mDNS queries observed. Best-effort — mdns-sd 0.11 does not
    /// expose incoming queries on a registered service directly, so this
    /// is incremented opportunistically and may stay 0 in practice.
    pub mdns_query_rx: AtomicU64,
    /// Count of `/Discover` POSTs received from Apple senders.
    pub discover_count: AtomicU64,
    /// Count of `/Ask` POSTs received.
    pub ask_count: AtomicU64,
    /// Count of `/Upload` POSTs received.
    pub upload_count: AtomicU64,
    /// Most recent peer's sender name (from `/Ask`), for "who tried us last".
    pub last_peer: Mutex<String>,
    /// Trace ring buffer; oldest events dropped past `TRACE_RING_CAP`.
    pub trace_ring: Mutex<VecDeque<TraceEvent>>,
    /// Whether the trace ring is accepting events (off by default).
    /// 0 = off, 1 = on.
    pub trace_enabled: AtomicU8,
    /// Current trace level setting (`"off"` / `"debug"` / `"trace"`), off by
    /// default. Stored for `/status` transparency and for re-applying on
    /// POST /trace. The atomic flag above is the hot-path gate.
    pub trace_level: Mutex<String>,
}

impl Default for IntrospectState {
    fn default() -> Self {
        Self {
            receiver_name: Mutex::new(String::new()),
            mdns_announced: AtomicBool::new(false),
            https_port: AtomicU16::new(0),
            last_announce_unix_ms: AtomicU64::new(0),
            mdns_query_rx: AtomicU64::new(0),
            discover_count: AtomicU64::new(0),
            ask_count: AtomicU64::new(0),
            upload_count: AtomicU64::new(0),
            last_peer: Mutex::new(String::new()),
            trace_ring: Mutex::new(VecDeque::new()),
            trace_enabled: AtomicU8::new(0),
            trace_level: Mutex::new("off".into()),
        }
    }
}

impl IntrospectState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a successful mDNS announce: sets the flag and the timestamp.
    pub fn record_announce(&self, receiver_name: String, https_port: u16) {
        if let Ok(mut n) = self.receiver_name.lock() {
            *n = receiver_name;
        }
        self.https_port.store(https_port, Ordering::Release);
        self.last_announce_unix_ms.store(now_unix_ms(), Ordering::Release);
        self.mdns_announced.store(true, Ordering::Release);
    }

    /// Bump the inbound mDNS-query counter by 1.
    pub fn bump_mdns_query_rx(&self) {
        self.mdns_query_rx.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a `/Discover` POST: bump the counter.
    pub fn bump_discover(&self) {
        self.discover_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Record an `/Ask` POST: bump the counter and stash the sender name.
    pub fn bump_ask(&self, sender: Option<&str>) {
        self.ask_count.fetch_add(1, Ordering::Relaxed);
        if let Some(s) = sender {
            if let Ok(mut cell) = self.last_peer.lock() {
                *cell = s.to_string();
            }
        }
    }

    /// Record an `/Upload` POST: bump the counter.
    pub fn bump_upload(&self) {
        self.upload_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Snapshot the status for `/status`. The ms-ago field is computed at
    /// snapshot time so callers don't have to.
    pub fn snapshot(&self) -> StatusSnapshot {
        let last_announce_ms = self.last_announce_unix_ms.load(Ordering::Acquire);
        let now = now_unix_ms();
        let last_announce_ms_ago = now.saturating_sub(last_announce_ms);
        let receiver_name = self
            .receiver_name
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default();
        let last_peer = self.last_peer.lock().map(|s| s.clone()).unwrap_or_default();
        StatusSnapshot {
            receiver_name,
            mdns_announced: self.mdns_announced.load(Ordering::Acquire),
            https_port: self.https_port.load(Ordering::Acquire),
            last_announce_ms_ago,
            mdns_query_rx: self.mdns_query_rx.load(Ordering::Acquire),
            discover_count: self.discover_count.load(Ordering::Acquire),
            ask_count: self.ask_count.load(Ordering::Acquire),
            upload_count: self.upload_count.load(Ordering::Acquire),
            last_peer,
        }
    }

    /// Push a trace event into the ring. **No-op when `trace_enabled == 0`**
    /// (the default) — the hot path pays only a single atomic load. When on,
    /// appends to the ring and drops the oldest entry past `TRACE_RING_CAP`.
    pub fn push_trace(&self, event: TraceEvent) {
        if self.trace_enabled.load(Ordering::Relaxed) == 0 {
            return;
        }
        if let Ok(mut ring) = self.trace_ring.lock() {
            while ring.len() >= TRACE_RING_CAP {
                ring.pop_front();
            }
            ring.push_back(event);
        }
    }

    /// Return the last `n` trace events (newest last). `n == 0` returns all.
    pub fn trace_last_n(&self, n: usize) -> Vec<TraceEvent> {
        if let Ok(ring) = self.trace_ring.lock() {
            if n == 0 {
                ring.iter().cloned().collect()
            } else {
                let start = ring.len().saturating_sub(n);
                ring.iter().skip(start).cloned().collect()
            }
        } else {
            Vec::new()
        }
    }

    /// Set the trace level (`"off"` / `"debug"` / `"trace"`). Anything other
    /// than `"off"` enables the ring; `"off"` disables it but does NOT clear
    /// the buffer — re-enabling surfaces previously captured events.
    pub fn set_trace_level(&self, level: &str) {
        let normalized = normalize_trace_level(level);
        let enabled = if normalized == "off" { 0 } else { 1 };
        self.trace_enabled.store(enabled, Ordering::Release);
        if let Ok(mut cell) = self.trace_level.lock() {
            *cell = normalized;
        }
    }

    /// Current trace level (`"off"` / `"debug"` / `"trace"`).
    pub fn trace_level(&self) -> String {
        self.trace_level
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|_| "off".into())
    }

    /// Whether the trace ring is currently accepting events.
    pub fn trace_enabled(&self) -> bool {
        self.trace_enabled.load(Ordering::Acquire) != 0
    }
}

/// Normalise a POSTed trace level to one of `off|debug|trace`. Unknown
/// values default to `"off"` (fail-safe: never silently enable tracing).
pub fn normalize_trace_level(level: &str) -> String {
    let l = level.trim().to_ascii_lowercase();
    match l.as_str() {
        "off" | "debug" | "trace" => l,
        _ => "off".into(),
    }
}

/// A `tracing` Layer that feeds structured events into the introspect ring
/// buffer when it's enabled. Off by default — `on_event` re-checks the
/// atomic flag every time so `POST /trace {"level":"debug"}` flips it on
/// without rebuilding the subscriber.
pub struct TraceCollector {
    pub state: Arc<IntrospectState>,
}

impl TraceCollector {
    pub fn new(state: Arc<IntrospectState>) -> Self {
        Self { state }
    }
}

impl<S> tracing_subscriber::Layer<S> for TraceCollector
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if self.state.trace_enabled.load(Ordering::Relaxed) == 0 {
            return;
        }
        self.state.push_trace(event_to_trace(event));
    }
}

/// Build a [`TraceEvent`] from a `tracing::Event` by visiting its recorded
/// fields. Pure (no I/O) so it's unit-testable in isolation.
pub fn event_to_trace(event: &tracing::Event<'_>) -> TraceEvent {
    let mut visitor = FieldVisitor::default();
    event.record(&mut visitor);
    let msg = visitor
        .message
        .take()
        .unwrap_or_else(|| event.metadata().name().to_string());
    TraceEvent {
        ts: now_unix_ms(),
        level: event.metadata().level().to_string(),
        target: event.metadata().target().to_string(),
        msg,
        fields: serde_json::Value::Object(visitor.fields),
    }
}

#[derive(Default)]
struct FieldVisitor {
    fields: serde_json::Map<String, serde_json::Value>,
    message: Option<String>,
}

impl tracing::field::Visit for FieldVisitor {
    fn record_debug(
        &mut self,
        field: &tracing::field::Field,
        value: &dyn std::fmt::Debug,
    ) {
        let formatted = format!("{:?}", value);
        if field.name() == "message" {
            self.message = Some(formatted.clone());
        }
        self.fields
            .insert(field.name().to_string(), serde_json::Value::String(formatted));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
        }
        self.fields
            .insert(field.name().to_string(), serde_json::Value::String(value.to_string()));
    }
}

/// A captured structured trace event in the ring buffer.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TraceEvent {
    pub ts: u64,
    pub level: String,
    pub target: String,
    pub msg: String,
    pub fields: serde_json::Value,
}

/// The JSON-serialisable `/status` body.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StatusSnapshot {
    pub receiver_name: String,
    pub mdns_announced: bool,
    pub https_port: u16,
    pub last_announce_ms_ago: u64,
    pub mdns_query_rx: u64,
    pub discover_count: u64,
    pub ask_count: u64,
    pub upload_count: u64,
    pub last_peer: String,
}

/// A produced introspection HTTP response: status, JSON body, content-type.
#[derive(Debug, Clone, PartialEq)]
pub struct IntrospectResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

impl IntrospectResponse {
    pub fn json(status: u16, value: &impl serde::Serialize) -> Self {
        Self {
            status,
            body: serde_json::to_vec(value).unwrap_or_else(|_| b"null".to_vec()),
        }
    }

    pub fn empty(status: u16) -> Self {
        Self { status, body: Vec::new() }
    }
}

/// Default introspection listen address (loopback only, port 9931).
pub const DEFAULT_HTTP_ADDR: &str = "127.0.0.1:9931";

/// Pure route dispatcher for the introspection API. Takes the request
/// method, the full request target (path + "?" + query), and the body, and
/// returns a JSON response. No I/O — fully unit-testable.
///
/// Routes:
/// - `GET /status` → 200 with [`StatusSnapshot`] as JSON.
/// - `GET /trace?n=300` → 200 with last N [`TraceEvent`]s as a JSON array
///   (default/missing `n` = 300; `n=0` = all; `n` capped at the ring cap).
/// - `POST /trace` with `{"level":"off|debug|trace"}` → 200, updates the
///   level gate. 400 on malformed JSON / missing `level` key.
/// - Anything else → 404.
pub fn route_introspect(
    state: &IntrospectState,
    method: &str,
    target: &str,
    body: &[u8],
) -> IntrospectResponse {
    let path = target.split('?').next().unwrap_or(target);
    match (method, path) {
        ("GET", "/status") => IntrospectResponse::json(200, &state.snapshot()),
        ("GET", "/trace") => {
            let n = parse_trace_n(target).unwrap_or(DEFAULT_TRACE_N);
            let events = state.trace_last_n(n);
            IntrospectResponse::json(200, &events)
        }
        ("POST", "/trace") => {
            let parsed: serde_json::Value = match serde_json::from_slice(body) {
                Ok(v) => v,
                Err(_) => return IntrospectResponse::json(400, &serde_json::json!({"error":"invalid json"})),
            };
            let level = parsed.get("level").and_then(|v| v.as_str());
            match level {
                Some(lv) => {
                    state.set_trace_level(lv);
                    IntrospectResponse::json(
                        200,
                        &serde_json::json!({"level": state.trace_level(), "enabled": state.trace_enabled()}),
                    )
                }
                None => IntrospectResponse::json(
                    400,
                    &serde_json::json!({"error":"missing 'level' key (expected off|debug|trace)"}),
                ),
            }
        }
        _ => IntrospectResponse::json(404, &serde_json::json!({"error":"not found"})),
    }
}

/// Default value used for `GET /trace` when no `n` query param is supplied.
pub const DEFAULT_TRACE_N: usize = 300;

/// Parse the `n` query param from a request target. Returns the parsed
/// value, or `None` if the param is absent. A non-numeric value parses to
/// `Some(0)` to mirror the ring-buffer semantics (`n=0` ⇒ all). Pure.
pub fn parse_trace_n(target: &str) -> Option<usize> {
    let q = target.split_once('?').map(|(_, q)| q)?;
    for pair in q.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k.eq_ignore_ascii_case("n") {
                return Some(v.parse().unwrap_or(0));
            }
        }
    }
    None
}

/// A bound introspection HTTP server. Drop the handle to let the thread wind
/// down (or call `stop()`); the listener is closed when the handle is
/// dropped.
pub struct IntrospectServer {
    pub local_addr: std::net::SocketAddr,
    pub handle: Option<std::thread::JoinHandle<()>>,
}

impl IntrospectServer {
    /// Politely stop the server. Best-effort: closing the listener is the
    /// real signal since `incoming()` errors out on a closed socket.
    pub fn stop(mut self) {
        self.handle.take(); // detach; thread ends on listener close
    }
}

/// Spawn the loopback introspection HTTP server. Binds `addr` (use
/// `DEFAULT_HTTP_ADDR` = `127.0.0.1:9931` for the production default, or
/// `127.0.0.1:0` for an ephemeral port in tests). Refuses anything that
/// isn't loopback (defence in depth: even if a caller passes a public
/// address we never expose the introspect API off-box).
pub fn spawn_http_server(
    addr: &str,
    state: Arc<IntrospectState>,
) -> std::io::Result<IntrospectServer> {
    let socket_addr: std::net::SocketAddr = addr
        .parse()
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("bad addr {addr}")))?;
    if !socket_addr.ip().is_loopback() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("refusing non-loopback introspect bind {addr} — loopback only"),
        ));
    }
    let listener = std::net::TcpListener::bind(socket_addr)?;
    let local_addr = listener.local_addr()?;
    // Explicit insurance against a logic regression in the parse path: even
    // if `addr` somehow passed the loopback check, the OS-chosen bind must
    // still be loopback or we abort before accepting a single connection.
    if !local_addr.ip().is_loopback() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("OS bound non-loopback addr {local_addr} — aborting"),
        ));
    }
    tracing::info!(addr = %local_addr, "introspect HTTP server listening (loopback)");
    let handle = std::thread::Builder::new()
        .name("luftlift-introspect".into())
        .spawn(move || serve_loop(&listener, state))?;
    Ok(IntrospectServer { local_addr, handle: Some(handle) })
}

fn serve_loop(listener: &std::net::TcpListener, state: Arc<IntrospectState>) {
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                if let Err(e) = handle_introspect_connection(s, &state) {
                    tracing::warn!(error = %e, "introspect connection error");
                }
            }
            Err(e) => {
                // EBADF after listener close is expected during shutdown.
                if e.kind() != std::io::ErrorKind::Other {
                    tracing::warn!(error = %e, "introspect accept error");
                }
            }
        }
    }
}

/// Handle one HTTP/1.x connection: parse request line + headers, read body
/// by Content-Length, dispatch via [`route_introspect`], write the JSON
/// response. Returns the parse/dispatch result; the connection is closed
/// after a single request (no keep-alive — the introspect client is `curl`).
fn handle_introspect_connection(
    stream: std::net::TcpStream,
    state: &IntrospectState,
) -> std::io::Result<()> {
    use std::io::{BufRead, BufReader, Read, Write};

    let mut reader = BufReader::new(stream);
    let mut req_line = String::new();
    if reader.read_line(&mut req_line)? == 0 {
        return Ok(()); // empty connection — drop quietly
    }
    let mut parts = req_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");

    // Read headers up to the blank line.
    let mut content_length: usize = 0;
    loop {
        let mut h = String::new();
        let n = reader.read_line(&mut h)?;
        if n == 0 || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
    }

    // Read body.
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body)?;
    }

    let resp = route_introspect(state, method, target, &body);
    let reason = match resp.status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "OK",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {cl}\r\n\
         Connection: close\r\n\
         Access-Control-Allow-Origin: *\r\n\
         \r\n",
        status = resp.status,
        cl = resp.body.len(),
    );
    let stream = reader.get_mut();
    stream.write_all(head.as_bytes())?;
    stream.write_all(&resp.body)?;
    stream.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_defaults_are_zero_and_unannounced() {
        let st = IntrospectState::new();
        let s = st.snapshot();
        assert!(!s.mdns_announced, "default state is not announced");
        assert_eq!(s.https_port, 0);
        assert_eq!(s.mdns_query_rx, 0);
        assert_eq!(s.discover_count, 0);
        assert_eq!(s.ask_count, 0);
        assert_eq!(s.upload_count, 0);
        assert_eq!(s.receiver_name, "");
        assert_eq!(s.last_peer, "");
    }

    #[test]
    fn record_announce_sets_name_port_flag_and_recent_timestamp() {
        let st = IntrospectState::new();
        st.record_announce("luftlift-42".into(), 8771);
        let s = st.snapshot();
        assert!(s.mdns_announced);
        assert_eq!(s.receiver_name, "luftlift-42");
        assert_eq!(s.https_port, 8771);
        assert!(s.last_announce_ms_ago < 5_000, "ms_ago must be tiny after announce");
    }

    #[test]
    fn counters_bump_independently() {
        let st = IntrospectState::new();
        st.bump_discover();
        st.bump_discover();
        st.bump_discover();
        st.bump_ask(Some("iPhone"));
        st.bump_upload();
        st.bump_upload();
        st.bump_mdns_query_rx();
        st.bump_mdns_query_rx();
        let s = st.snapshot();
        assert_eq!(s.discover_count, 3);
        assert_eq!(s.ask_count, 1);
        assert_eq!(s.upload_count, 2);
        assert_eq!(s.mdns_query_rx, 2);
    }

    #[test]
    fn bump_ask_updates_last_peer() {
        let st = IntrospectState::new();
        st.bump_ask(Some("Andrew's iPhone"));
        assert_eq!(st.snapshot().last_peer, "Andrew's iPhone");
        st.bump_ask(None);
        assert_eq!(st.snapshot().last_peer, "Andrew's iPhone");
        st.bump_ask(Some("MacBook"));
        assert_eq!(st.snapshot().last_peer, "MacBook");
    }

    #[test]
    fn snapshot_is_a_point_in_time_copy() {
        let st = IntrospectState::new();
        st.bump_discover();
        let s = st.snapshot();
        st.bump_discover();
        st.bump_discover();
        assert_eq!(s.discover_count, 1, "snapshot is a copy");
        assert_eq!(st.snapshot().discover_count, 3);
    }

    #[test]
    fn status_snapshot_serialises_to_json_with_expected_fields() {
        let st = IntrospectState::new();
        st.record_announce("luftlift".into(), 8771);
        st.bump_discover();
        let json = serde_json::to_value(st.snapshot()).unwrap();
        let obj = json.as_object().expect("status is a JSON object");
        for key in [
            "receiver_name",
            "mdns_announced",
            "https_port",
            "last_announce_ms_ago",
            "mdns_query_rx",
            "discover_count",
            "ask_count",
            "upload_count",
            "last_peer",
        ] {
            assert!(obj.contains_key(key), "/status JSON must include field {key}");
        }
        assert_eq!(obj["discover_count"], serde_json::json!(1));
        assert_eq!(obj["mdns_announced"], serde_json::json!(true));
    }

    // ---- trace ring + off-by-default gate ----

    fn ev(msg: &str) -> TraceEvent {
        TraceEvent {
            ts: 0,
            level: "INFO".into(),
            target: "test".into(),
            msg: msg.into(),
            fields: serde_json::Value::Null,
        }
    }

    #[test]
    fn trace_off_by_default_push_is_noop() {
        let st = IntrospectState::new();
        assert!(!st.trace_enabled(), "trace must be off by default");
        assert_eq!(st.trace_level(), "off");
        st.push_trace(ev("a"));
        assert!(st.trace_last_n(0).is_empty(), "push must be a no-op when off");
    }

    #[test]
    fn trace_enabled_push_appends_and_returns_in_order() {
        let st = IntrospectState::new();
        st.set_trace_level("debug");
        assert!(st.trace_enabled());
        assert_eq!(st.trace_level(), "debug");
        st.push_trace(ev("a"));
        st.push_trace(ev("b"));
        st.push_trace(ev("c"));
        let events = st.trace_last_n(0);
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].msg, "a");
        assert_eq!(events[1].msg, "b");
        assert_eq!(events[2].msg, "c");
    }

    #[test]
    fn trace_last_n_returns_tail_respecting_n() {
        let st = IntrospectState::new();
        st.set_trace_level("trace");
        for i in 0..5 {
            st.push_trace(ev(&format!("e{i}")));
        }
        assert_eq!(st.trace_last_n(0).len(), 5, "n == 0 returns all");
        let last3 = st.trace_last_n(3);
        assert_eq!(last3.len(), 3);
        assert_eq!(last3[0].msg, "e2");
        assert_eq!(last3[2].msg, "e4");
        // n larger than the ring returns everything.
        assert_eq!(st.trace_last_n(100).len(), 5);
    }

    #[test]
    fn trace_ring_drops_oldest_past_cap() {
        // Sanity: cap is the documented ~2000.
        assert_eq!(TRACE_RING_CAP, 2000);
        let st = IntrospectState::new();
        st.set_trace_level("debug");
        for i in 0..(TRACE_RING_CAP + 50) {
            st.push_trace(ev(&format!("e{i}")));
        }
        let all = st.trace_last_n(0);
        assert_eq!(all.len(), TRACE_RING_CAP, "ring must be capped");
        // Oldest entries (e0..e49) were dropped; newest first kept is e50.
        assert_eq!(all.first().unwrap().msg, "e50", "oldest in ring is e50");
        assert_eq!(all.last().unwrap().msg, format!("e{}", TRACE_RING_CAP + 49));
    }

    #[test]
    fn trace_set_level_off_disables_but_does_not_clear_buffer() {
        let st = IntrospectState::new();
        st.set_trace_level("debug");
        st.push_trace(ev("captured"));
        assert_eq!(st.trace_last_n(0).len(), 1);
        st.set_trace_level("off");
        assert!(!st.trace_enabled());
        // Push is a no-op again.
        st.push_trace(ev("ignored"));
        // But previously captured events are still retrievable.
        assert_eq!(st.trace_last_n(0).len(), 1);
        assert_eq!(st.trace_last_n(0)[0].msg, "captured");
    }

    #[test]
    fn trace_normalize_level_accepts_off_debug_trace_rejects_unknown() {
        assert_eq!(normalize_trace_level("off"), "off");
        assert_eq!(normalize_trace_level("DEBUG"), "debug");
        assert_eq!(normalize_trace_level("trace"), "trace");
        assert_eq!(normalize_trace_level("  debug  "), "debug");
        // Unknown values fail-safe to "off" — never silently enable tracing.
        assert_eq!(normalize_trace_level("verbose"), "off");
        assert_eq!(normalize_trace_level(""), "off");
    }

    #[test]
    fn trace_set_level_unknown_fails_safe_to_off() {
        let st = IntrospectState::new();
        st.set_trace_level("debug");
        st.set_trace_level("nonsense");
        assert!(!st.trace_enabled(), "unknown level must disable tracing");
        st.push_trace(ev("x"));
        assert!(st.trace_last_n(0).is_empty());
    }

    #[test]
    fn trace_event_serialises_to_json_with_expected_shape() {
        let e = TraceEvent {
            ts: 1_700_000_000_000,
            level: "INFO".into(),
            target: "luftlift_rs::server".into(),
            msg: "Discover request".into(),
            fields: serde_json::json!({"peer": "fe80::1"}),
        };
        let v = serde_json::to_value(&e).unwrap();
        let obj = v.as_object().unwrap();
        for key in ["ts", "level", "target", "msg", "fields"] {
            assert!(obj.contains_key(key), "TraceEvent JSON must include {key}");
        }
        assert_eq!(obj["fields"]["peer"], serde_json::json!("fe80::1"));
    }

    // ---- TraceCollector layer (integration with tracing) ----

    #[test]
    fn trace_collector_captures_events_only_when_enabled() {
        use std::sync::Arc;
        use tracing_subscriber::layer::SubscriberExt;

        let state = Arc::new(IntrospectState::new());
        let collector = TraceCollector::new(state.clone());
        let subscriber = tracing_subscriber::FmtSubscriber::new().with(collector);

        tracing::subscriber::with_default(subscriber, || {
            // OFF by default — emit two events, expect zero captured.
            tracing::info!("off message one");
            tracing::warn!(peer = "fe80::1", "off message two");
            assert_eq!(state.trace_last_n(0).len(), 0);

            // Flip ON and emit more events.
            state.set_trace_level("debug");
            tracing::info!(peer = "10.0.0.1", port = 8771, "Discover request");
            tracing::warn!("no fields here");
        });

        let events = state.trace_last_n(0);
        assert_eq!(events.len(), 2, "only the two post-enable events captured");
        // Level + target + msg are extracted from event metadata.
        assert_eq!(events[0].level, "INFO");
        assert_eq!(events[0].msg, "Discover request");
        assert_eq!(events[1].level, "WARN");
        assert_eq!(events[1].msg, "no fields here");
        // Structured fields survive as JSON.
        let f0 = events[0].fields.as_object().expect("fields is object");
        assert_eq!(f0.get("peer").and_then(|v| v.as_str()), Some("10.0.0.1"));
        let port_v = f0.get("port").expect("port field present");
        let port_n: i64 = port_v.as_i64().or_else(|| port_v.as_str().and_then(|s| s.parse().ok())).expect("port numeric");
        assert_eq!(port_n, 8771);
    }

    #[test]
    fn trace_collector_uses_target_from_event_metadata() {
        use std::sync::Arc;
        use tracing_subscriber::layer::SubscriberExt;

        let state = Arc::new(IntrospectState::new());
        let collector = TraceCollector::new(state.clone());
        let subscriber = tracing_subscriber::FmtSubscriber::new().with(collector);

        tracing::subscriber::with_default(subscriber, || {
            state.set_trace_level("trace");
            tracing::info!("hello target");
        });
        let events = state.trace_last_n(0);
        assert_eq!(events.len(), 1);
        assert!(
            events[0].target.contains("introspect"),
            "target was {:?}, expected to contain 'introspect'",
            events[0].target
        );
    }

    // ---- route_introspect: pure HTTP dispatch ----

    #[test]
    fn route_get_status_returns_snapshot_json() {
        let st = IntrospectState::new();
        st.record_announce("luftlift".into(), 8771);
        st.bump_discover();
        let resp = route_introspect(&st, "GET", "/status", b"");
        assert_eq!(resp.status, 200);
        let v: serde_json::Value = serde_json::from_slice(&resp.body).unwrap();
        assert_eq!(v["receiver_name"], serde_json::json!("luftlift"));
        assert_eq!(v["https_port"], serde_json::json!(8771));
        assert_eq!(v["discover_count"], serde_json::json!(1));
        assert_eq!(v["mdns_announced"], serde_json::json!(true));
    }

    #[test]
    fn route_get_trace_returns_array_with_default_n_when_no_param() {
        let st = IntrospectState::new();
        st.set_trace_level("debug");
        for i in 0..400 {
            st.push_trace(ev(&format!("e{i}")));
        }
        // No n= param: default returns DEFAULT_TRACE_N (300) events, newest.
        let resp = route_introspect(&st, "GET", "/trace", b"");
        assert_eq!(resp.status, 200);
        let arr: serde_json::Value = serde_json::from_slice(&resp.body).unwrap();
        let arr = arr.as_array().unwrap();
        assert_eq!(arr.len(), DEFAULT_TRACE_N);
        // Last (newest) is e399; first (oldest kept) is e100.
        assert_eq!(arr.last().unwrap()["msg"], serde_json::json!("e399"));
        assert_eq!(arr.first().unwrap()["msg"], serde_json::json!("e100"));
    }

    #[test]
    fn route_get_trace_respects_n_query_param() {
        let st = IntrospectState::new();
        st.set_trace_level("debug");
        for i in 0..10 {
            st.push_trace(ev(&format!("e{i}")));
        }
        let resp = route_introspect(&st, "GET", "/trace?n=3", b"");
        assert_eq!(resp.status, 200);
        let arr: serde_json::Value = serde_json::from_slice(&resp.body).unwrap();
        let arr = arr.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["msg"], serde_json::json!("e7"));
        assert_eq!(arr[2]["msg"], serde_json::json!("e9"));
    }

    #[test]
    fn route_get_trace_n_zero_returns_all() {
        let st = IntrospectState::new();
        st.set_trace_level("debug");
        for i in 0..5 {
            st.push_trace(ev(&format!("e{i}")));
        }
        let resp = route_introspect(&st, "GET", "/trace?n=0", b"");
        assert_eq!(resp.status, 200);
        let arr: serde_json::Value = serde_json::from_slice(&resp.body).unwrap();
        assert_eq!(arr.as_array().unwrap().len(), 5);
    }

    #[test]
    fn route_post_trace_debug_enables_ring() {
        let st = IntrospectState::new();
        assert!(!st.trace_enabled());
        let resp = route_introspect(&st, "POST", "/trace", br#"{"level":"debug"}"#);
        assert_eq!(resp.status, 200);
        assert!(st.trace_enabled());
        assert_eq!(st.trace_level(), "debug");
    }

    #[test]
    fn route_post_trace_off_disables_ring() {
        let st = IntrospectState::new();
        st.set_trace_level("debug");
        let resp = route_introspect(&st, "POST", "/trace", br#"{"level":"off"}"#);
        assert_eq!(resp.status, 200);
        assert!(!st.trace_enabled());
    }

    #[test]
    fn route_post_trace_bad_json_returns_400() {
        let st = IntrospectState::new();
        let resp = route_introspect(&st, "POST", "/trace", b"not json");
        assert_eq!(resp.status, 400);
        // Level must NOT have changed.
        assert_eq!(st.trace_level(), "off");
    }

    #[test]
    fn route_post_trace_missing_level_key_returns_400() {
        let st = IntrospectState::new();
        let resp = route_introspect(&st, "POST", "/trace", br#"{"foo":"bar"}"#);
        assert_eq!(resp.status, 400);
        assert!(!st.trace_enabled());
    }

    #[test]
    fn route_unknown_path_returns_404() {
        let st = IntrospectState::new();
        let resp = route_introspect(&st, "GET", "/nope", b"");
        assert_eq!(resp.status, 404);
    }

    #[test]
    fn route_wrong_method_on_status_returns_404() {
        let st = IntrospectState::new();
        let resp = route_introspect(&st, "POST", "/status", b"");
        assert_eq!(resp.status, 404);
    }

    #[test]
    fn parse_trace_n_returns_value_for_valid_n() {
        assert_eq!(parse_trace_n("/trace?n=10"), Some(10));
        assert_eq!(parse_trace_n("/trace?n=0"), Some(0));
        assert_eq!(parse_trace_n("/trace?foo=1&n=42"), Some(42));
        assert_eq!(parse_trace_n("/trace?n=300&other=1"), Some(300));
    }

    #[test]
    fn parse_trace_n_returns_none_when_absent() {
        assert_eq!(parse_trace_n("/trace"), None);
        assert_eq!(parse_trace_n("/trace?foo=1"), None);
    }

    #[test]
    fn parse_trace_n_garbage_value_parses_to_zero() {
        // Match ring-buffer semantics: n=0 returns all. Non-numeric values
        // also parse to 0 to fail safe (broad rather than narrow capture).
        assert_eq!(parse_trace_n("/trace?n=abc"), Some(0));
    }

    // ---- HTTP server (real loopback TCP integration) ----

    #[test]
    fn http_server_get_status_serves_snapshot_json() {
        use std::io::{Read, Write};
        use std::net::TcpStream;

        let state = Arc::new(IntrospectState::new());
        state.record_announce("luftlift".into(), 8771);
        state.bump_discover();
        let server = spawn_http_server("127.0.0.1:0", state.clone()).expect("bind");
        assert!(server.local_addr.ip().is_loopback(), "must bind loopback only");

        let mut s = TcpStream::connect(server.local_addr).expect("connect");
        s.write_all(b"GET /status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .expect("write");
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).expect("read");
        let resp = String::from_utf8_lossy(&buf);
        assert!(resp.starts_with("HTTP/1.1 200"), "status line: {}", &resp[..40]);
        assert!(resp.contains("Content-Type: application/json"));
        let body_start = resp.find("\r\n\r\n").expect("header terminator") + 4;
        let json: serde_json::Value =
            serde_json::from_slice(&buf[body_start..]).expect("body is JSON");
        assert_eq!(json["receiver_name"], serde_json::json!("luftlift"));
        assert_eq!(json["https_port"], serde_json::json!(8771));
        assert_eq!(json["discover_count"], serde_json::json!(1));
    }

    #[test]
    fn http_server_post_trace_enables_ring_and_get_trace_returns_events() {
        use std::io::{Read, Write};
        use std::net::TcpStream;

        let state = Arc::new(IntrospectState::new());
        let server = spawn_http_server("127.0.0.1:0", state.clone()).expect("bind");

        // 1. POST /trace to enable.
        let mut s = TcpStream::connect(server.local_addr).expect("connect");
        let body = br#"{"level":"debug"}"#;
        let req = format!(
            "POST /trace HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        s.write_all(req.as_bytes()).expect("write head");
        s.write_all(body).expect("write body");
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).expect("read");
        let resp = String::from_utf8_lossy(&buf);
        assert!(resp.starts_with("HTTP/1.1 200"), "POST /trace response: {}", &resp[..40]);
        assert!(state.trace_enabled(), "ring must be enabled after POST /trace");

        // 2. Emit an event from inside the dispatcher thread so the layer
        //    observes it.
        use tracing_subscriber::layer::SubscriberExt;
        let collector = TraceCollector::new(state.clone());
        let subscriber = tracing_subscriber::FmtSubscriber::new().with(collector);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(peer = "10.0.0.1", "Discover request");
        });

        // 3. GET /trace and verify the event is in the response.
        let mut s = TcpStream::connect(server.local_addr).expect("connect 2");
        s.write_all(b"GET /trace HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .expect("write get");
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).expect("read 2");
        let resp = String::from_utf8_lossy(&buf);
        assert!(resp.starts_with("HTTP/1.1 200"));
        let body_start = resp.find("\r\n\r\n").expect("hdr term") + 4;
        let arr: serde_json::Value =
            serde_json::from_slice(&buf[body_start..]).expect("trace body is JSON");
        let events = arr.as_array().expect("trace body is array");
        assert!(!events.is_empty(), "at least one event captured");
        assert_eq!(events.last().unwrap()["msg"], serde_json::json!("Discover request"));
    }

    #[test]
    fn http_server_unknown_path_returns_404() {
        use std::io::{Read, Write};
        use std::net::TcpStream;

        let state = Arc::new(IntrospectState::new());
        let server = spawn_http_server("127.0.0.1:0", state).expect("bind");
        let mut s = TcpStream::connect(server.local_addr).expect("connect");
        s.write_all(b"GET /nope HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .expect("write");
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).expect("read");
        let resp = String::from_utf8_lossy(&buf);
        assert!(resp.starts_with("HTTP/1.1 404"), "got: {}", &resp[..40]);
    }

    #[test]
    fn http_server_post_trace_bad_json_returns_400() {
        use std::io::{Read, Write};
        use std::net::TcpStream;

        let state = Arc::new(IntrospectState::new());
        let server = spawn_http_server("127.0.0.1:0", state).expect("bind");
        let mut s = TcpStream::connect(server.local_addr).expect("connect");
        let body = b"not json";
        let req = format!(
            "POST /trace HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        s.write_all(req.as_bytes()).expect("write head");
        s.write_all(body).expect("write body");
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).expect("read");
        let resp = String::from_utf8_lossy(&buf);
        assert!(resp.starts_with("HTTP/1.1 400"), "got: {}", &resp[..40]);
    }

    #[test]
    fn http_server_refuses_non_loopback_bind() {
        // Defence in depth: even if a caller asks for an off-box bind, we
        // refuse — the introspect API must never be reachable over AWDL.
        let state = Arc::new(IntrospectState::new());
        let result = spawn_http_server("0.0.0.0:0", state);
        assert!(result.is_err(), "must refuse to bind a non-loopback address");
    }
}
