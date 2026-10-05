# PORT.md — `luftlift-rs` (port of opendrop)  — owner: GLM-5.2

Goal: a Rust port of SEEMOO's **opendrop** (the AirDrop application layer). It
runs over the `awdl0` interface (brought up by the separate `filin-rs` crate) and
interoperates with **real Apple devices** — we have verified the patched Python
opendrop receiving a photo from an iPhone 15 Pro Max on **iOS 18.6.2**.

**You don't touch filin-rs.** Your contract is: bind sockets to `awdl0` (an IPv6
link-local netdev). For pure-logic dev/tests you don't even need hardware.

Reference: `../../opendrop-patch/` — the patched Python `server.py` (read it; it
already contains the iOS-18 fixes) and `dvzip.py` (the format decoder). Port that
behavior to Rust.

## Subcommands (match opendrop)
`luftlift-rs -i awdl0 receive`
`luftlift-rs -i awdl0 find`
`luftlift-rs -i awdl0 send -r <id> -f <file>`

## Components to port
1. **mDNS over awdl0**: announce `_airdrop._tcp` (for `receive`) and browse it
   (for `find`/`send`). IPv6 link-local, port 8771. Apple devices appear/announce
   only while their sharing pane is open — document that.
2. **TLS**: an HTTPS server + client. opendrop uses a self-signed cert; Apple's
   cert chain validation is lenient for AirDrop. Use rustls.
3. **HTTP handlers** (`/Discover`, `/Ask`, `/Upload`) — the AirDrop protocol:
   - **iOS-18 quirk**: `/Discover` and `/Ask` POSTs use **chunked** transfer
     encoding with **no `Content-Length`**. A naive `int(Content-Length)` crashes
     (this was opendrop's bug #1). Read chunked bodies.
   - `/Discover` → respond with our record (ReceiverComputerName, media caps) so
     we appear in the sender's picker.
   - `/Ask` → the request plist describes the file(s); auto-accept (200) with our
     receiver info. **Links** arrive as a `.webloc` file described here (no special
     URL field — surface the link by parsing the eventual webloc/CPIO).
   - `/Upload` → the file payload.
4. **Payload formats** (the part with real reverse-engineering — see
   `../../docs/airdrop-protocol.md`):
   - Content-Type **`application/x-cpio`** (older) OR **`application/x-dvzip`** (iOS 18).
   - **dvzip**: a sequence of framed zlib blocks `[u32 big-endian length][zlib
     stream]`; inflate+concatenate → an **ODC CPIO** archive (magic `070707`)
     containing the file(s). Reference decoder: `../../opendrop-patch/dvzip.py`.
     **Fixture available:** a real captured dvzip at `/tmp/upload.bin` (317 KB,
     contains FullSizeRender.jpg) — copy it to `tests/fixtures/` and assert your
     decoder extracts the JPEG byte-for-byte.
   - CPIO (ODC) extraction; gzip for the legacy path.
5. **plist**: parse/emit Apple binary + XML plists (Discover/Ask request+response).

## TDD order (great because the formats are pure functions with real fixtures)
1. **dvzip decoder** — RED: test that `dvzip_to_cpio(fixture)` yields bytes whose
   magic is `070707` and length matches; GREEN: implement the framed-zlib loop.
   Then: extract the JPEG and assert it's a valid JPEG of the known size (317435 B).
2. **CPIO (ODC) reader** — extract entries (name + bytes) from the decoded archive.
3. **chunked body reader** — RED with a hand-built chunked body (incl. the
   no-Content-Length case); GREEN.
4. **plist** Discover/Ask request parse + response emit (use captured plists; we
   have a real Ask plist structure documented in docs/airdrop-protocol.md).
5. Wire mDNS + TLS + HTTP handlers; integration-test `receive` against the Python
   opendrop or a real Apple device over awdl0.

## Debuggability (primary goal — better than opendrop)
`tracing` structured events: `mdns_announce`/`mdns_discovered` { name, addr },
`http_request` { peer, method, path, status }, `ask` { sender, files }, `upload`
{ bytes, mbps }, `link_received` { url }. A `find` should clearly list discovered
peers; a `receive` should narrate the Discover→Ask→Upload phases. The Python
stack hid all this at debug level — surface it.

## Loopback introspection API (`luftlift receive --http-addr`)
While `receive` runs, a tiny **loopback-only** HTTP/JSON server exposes live
receiver state so the orchestrator can `curl | jq` instead of grepping
`/tmp/lrF.log`. It is SEPARATE from the AirDrop HTTPS listener on 8771 —
different port, different server thread, never reachable off-box.

- **Flag:** `--http-addr 127.0.0.1:9931` (default). Pass `--http-addr off` to
  disable it entirely. The bind is hard-refused if the resolved IP isn't
  loopback (defence in depth against an accidental off-box bind).
- **Endpoints (JSON, `application/json`):**
  - `GET /status` →
    `{ receiver_name, mdns_announced, https_port, last_announce_ms_ago,
       mdns_query_rx, discover_count, ask_count, upload_count, last_peer }`.
    The Discover/Ask/Upload counters let you see how far the iPhone got
    without tailing the log.
  - `GET /trace?n=300` → last N structured trace events from a 2000-cap ring
    buffer (default N=300; `n=0` returns all). Each event is
    `{ ts, level, target, msg, fields }`.
  - `POST /trace` with `{"level":"off|debug|trace"}` → toggles the atomic
    gate that pushes events into the ring. **Off by default** (so the hot
    path is just an atomic load). Anything other than `off` enables the
    ring; `"off"` disables it but does NOT clear the buffer.

Implementation lives in `src/introspect.rs`: `IntrospectState` holds the
atomic counters + ring + gate, a custom `tracing`-subscriber `Layer`
(`TraceCollector`) feeds events into the ring when enabled, and a small
`std::net::TcpListener` server serves the three endpoints. The receiver
bumps the counters in `server::bump_introspect_for_request` on every
AirDrop POST.

## Workflow
Follow `../../WORKFLOW.md`: work on `main`, **only under `crates/luftlift-rs/`**,
TDD with red/green/review commits. Coordinate workspace `Cargo.toml` dep
additions with Claude.
