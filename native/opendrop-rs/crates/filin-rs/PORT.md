# PORT.md — `filin-rs` (port of owl)  — owner: gpt-5.5

Goal: a Rust port of SEEMOO's **owl** AWDL daemon. It puts a Wi-Fi monitor
interface to work, speaks Apple Wireless Direct Link, and brings up an `awdl0`
network interface that the separate `luftlift-rs` crate uses via normal sockets.
**You do not need to touch luftlift-rs** — the only contract is the `awdl0`
netdev (IPv6 link-local, MTU 1450).

Reference C source is in `../../owl/` (read it). Our key fix is in
`owl/src/rx.c` (hardware-TSF timestamping) — port that behavior from the start,
not owl's buggy original.

## What owl does (and you must port)
1. **Interface setup**: take a Wi-Fi iface already in plain monitor mode (we run
   it externally; do NOT try to set the `NL80211_MNTR_FLAG_ACTIVE` flag — many
   drivers reject it, this was owl's `-N` mode). Open it for RX+TX (pcap or
   AF_PACKET). Create the host tun `awdl0` with the same ether addr.
2. **RX path** (`src/rx.c`): parse radiotap → 802.11 → AWDL action frames.
   Handle PSF (Periodic Sync Frame) and MIF (Master Indication Frame) and their
   TLVs (sync params, election params, channel sequence, service params, data path).
   De-encapsulate AWDL data frames and write them to `awdl0`.
3. **Election** (`src/election.c`): track the AWDL master election tree; adopt the
   highest-metric master; expose the current master + metric.
4. **Synchronization** (`src/sync.c`) — **THE critical part**:
   - Compute availability-window timing from the master's announced
     `time_to_next_aw` + `aw_counter`.
   - **Timestamp received frames with the radiotap TSFT (hardware RX time), NOT a
     userspace clock.** Bridge TSF→monotonic with a filtered offset (EMA + sanity
     clamp). This is the single most important correctness item — see
     `../../docs/awdl-sync.md` and our `owl/src/rx.c` patch. Target sync error well
     under the AWDL guard window; on a good radio (Atheros) we measured ~0.4%.
5. **Channel hopping** (`src/channel.c`, `daemon/core.c`): follow the master's
   channel sequence; default anchor **channel 44**. Use nl80211 set_channel.
6. **TX**: periodically send our own PSF/MIF; inject AWDL-framed data from `awdl0`.
   Injection can **EAGAIN** under load — size the send buffer (SO_SNDBUF) and
   queue/retry rather than drop (owl just dropped → ~32% loss on USB).

## CLI (match owl where sensible)
`filin-rs -i <wlan> [-c 44] [-h awdl0] [-N] [-f] [-v...]`
- `-i` monitor iface, `-c` channel (6/44/149), `-h` host iface name (awdl0),
  `-N` assume already-monitor, `-f` disable RSSI filter.

## Debuggability (a primary goal — better than owl)
Use `tracing` with structured fields. Emit events:
- `peer_added`/`peer_removed` { addr, rssi }
- `election_changed` { self, master, metric, hops }
- `sync_error` { error_tu, pct, master } — every measurement, not just over-threshold
- `channel_switch` { from, to }
- `tx_inject` { ok, eagain_count }
Provide `--json` logs and ideally a small status endpoint or periodic summary
line (peers / master / sync% / channel). The Python/C stack made "why no
discovery?" agony — make it observable here.

## Milestones
1. iface setup + awdl0 up + radiotap/802.11/AWDL frame parsing (RX), structured peer logging.
2. Election + sync (hardware TSF) — log sync% so we can verify against the C owl.
3. Channel hopping + TX (PSF/MIF + data inject with backpressure handling).
4. Validate: with a real Apple device active, `luftlift-rs` (other crate) should
   discover and receive a file over the awdl0 this brings up.

## Localhost HTTP introspection API (`--http-addr`)
A loopback-only HTTP/JSON server for live debugging — `curl | jq` instead of
grepping `/tmp/fF.log`. See `FILIN_HTTP_INTROSPECT.md` for the full rationale.

- **CLI**: `--http-addr 127.0.0.1:9930` (default), or `--no-http` to disable.
  Non-loopback addresses are rejected at config time (`ConfigError::NonLoopbackHttpAddr`)
  and the server additionally checks each connection's peer address.
- **Threading**: the server runs on its own OS thread; the AF_PACKET hot loop
  only bumps `AtomicU64` counters and (throttled ~10 Hz) refreshes an `RwLock`
  snapshot — it never takes a lock on the hot path.
- **Endpoints** (JSON):
  - `GET /status` → `current_channel`, `master_mac`, `master_seq`,
    `synced`, `peer_count`, `master_anchor`, `rebroadcast_counts: {"<chan>":N,...}`
    (adaptive — includes the anchor), `mdns_rx_self`, `mdns_rx_other`. The
    `mdns_rx_other` counter is the direct "are we hearing the iPhone at all?"
    signal.
  - `GET /peers` → array of `{ mac, last_seen_ms_ago, decoded_chanseq, current_channel }`.
  - `GET /trace?n=300` → last N structured trace events from a 2000-cap ring.
  - `POST /trace` with `{"level":"off|debug|trace"}` → flips the `AtomicU8` that
    gates whether the custom `tracing` Layer pushes events into the ring (off
    by default, so the hot path is one relaxed load + compare).

## Adaptive anchor re-broadcast + dwell (FILIN_ANCHOR_REBROADCAST.md)
On a cluster whose master anchors on a non-social channel (e.g. **52** DFS),
filin previously only re-broadcast the cached announce on the hardcoded
6/44/149 and barely dwelled on 52 — the iPhone never heard us. filin now reads
the **anchor** from the adopted master's decoded channel sequence (slot-0 /
dominant channel) at runtime and:

1. **Re-broadcasts** the cached mDNS announce on the anchor in addition to
   6/44/149 (`rebroadcast_channels(anchor)` → `AnnounceCache::rebroadcast`
   allowed set). The `/status` `rebroadcast_counts` map now tracks any channel
   (e.g. `"52"`), not just the three socials.
2. **Dwells** on the anchor: `ensure_social_coverage(seq, anchor)` fills null
   slots alternating anchor / social so the anchor gets ~50% of fills —
   materially more than the bare ~6% from the master's sequence alone.

Key types: `schedule::master_anchor_channel`, `schedule::rebroadcast_channels`,
`state::AwdlState::master_anchor`. All read at runtime — no hardcoded channel
numbers beyond the AWDL social baseline.

## Sync-quality metrics + master-adoption hysteresis (FILIN_SYNC_QUALITY.md)
Live iPhone debugging showed the AWDL cluster here has **5 Apple devices churning
as master**. Key refinement: what filin logs as "master" is `election.sync_addr`
(our PARENT peer), NOT `election.master_addr` (the cluster's TOP master). The
apparent churn is re-PARENTING among peers sharing one `master_addr`.

**/status now distinguishes both** (Part A):
- `master_mac` = cluster top master (`master_addr`, the stable one).
- `sync_addr_mac` = our direct parent (`sync_addr`, which re-parents as filin
  hops and hears different peer subsets).
- `master_addr_changes` / `sync_addr_changes` — separate change counters so you
  can prove `master_addr` is stable while `sync_addr` churns.
- `tsf_offset_us`, `master_age_ms`, `aw_alignment_pct`, `last_master_macs` —
  the rest of the sync-quality surface.

**Single-channel park mode** (the bigger pivot; opt-in via `--park`):
- The carl9170 is a DEDICATED monitor — it doesn't share a radio with infra like
  a real Apple device, so hopping is only needed to time-share, which we don't
  need to do. `--park` makes filin **stop hopping** and stay fixed on ONE
  channel = the stable top master's anchor (`park_channel()`, keyed off
  `master_addr` NOT the churning `sync_addr`). Maximizes presence on the
  iPhone's primary channel, always-listens (catches all inbound), and sidesteps
  the re-parenting channel thrash entirely. AW timing is kept so announces still
  transmit during availability windows. Default remains the hopping mode.

**Re-parent hysteresis**: the same-cluster re-parent guard was removed (it
froze the channel sequence and broke channel surfing, causing peer_count=0).
The existing Bug B debounce (`MASTER_DEBOUNCE_US`) handles single-frame
flip-flops. Use park mode (`--park`) for cluster-churn stability.

## IPv6 neighbor table population (FILIN_NEIGHBOR_TABLE.md)
**Likely THE root cause of AirDrop discovery failing past mDNS.** AWDL does
NOT use NDP. A node must derive each peer's link-local IPv6 from the source
MAC (RFC 4291 modified EUI-64) and install a static neighbor entry so unicast
awdl0 connections work. Without it, the Mac's `curl https://[our-v6]:8771/`
fails with "No route to host" and AirDrop never completes past mDNS.

- `awdl::link_local_ipv6(mac)` — pure RFC 4291 derivation (`fe80::(mac0 ^ 0x02):mac1:mac2:ff:fe:mac3:mac4:mac5`). Matches owl's `rfc4291_addr`.
- `state::NeighborTable` — dedup tracker: `note_peer_seen` returns `Add` the
  first time a MAC is sighted (then `NoChange`); `evict_stale(live)` returns
  `Remove` per evicted MAC. No netlink churn on re-frames.
- `os::rtnl::add_neighbor` / `remove_neighbor` — RTM_NEWNEIGH (NUD_PERMANENT)
  / RTM_DELNEIGH over NETLINK_ROUTE. Mirrors owl's `neighbor_add_rfc4291` but
  uses rtnetlink directly (owl shelled out to `ndp` on macOS).
- Wired into the hot loop: add on every parsed action frame, remove on
  `clean_peers` eviction. Errors are logged (warn) but non-fatal.

## Audit fixes vs owl + book (FILIN_AUDIT_FIXES.md)
Three fixes from an audit of filin vs owl (`owl/src/`) + book (docs/book/04,05):

**1. RSSI admission filter** (HIGH — fixes master flapping):
- owl drops weak frames before admitting/updating a peer (`rx.c:273-278`).
  filin now does the same: unknown peer needs `rssi >= -65`
  (`RSSI_THRESHOLD_DEFAULT`); known peer gets grace: `rssi >= -70`
  (threshold + `RSSI_GRACE_DEFAULT`). This hysteresis prevents marginal
  frames from causing election thrash.
- Pure decision fn: `rx::rssi_admits(rssi, peer_known, threshold, grace)`.
- CLI: `-f` / `--no-rssi-filter` disables the filter for testing (owl's `-f`).

**2. TSF saturating_sub** (MEDIUM — prevents underflow):
- `update_from_master`, `next_aw_tu`, and `current_aw` now use
  `saturating_sub` instead of unchecked `-`, matching `next_aw_us`. Prevents
  underflow early after start / after a TsftBridge re-lock.

**3. AWDL data-header magic** (LOW — fixes file transfer):
- `encapsulate_ethernet_payload` now writes `0x0403` (`AWDL_DATA_HEAD`,
  owl/src/tx.c:33) instead of `0x0000`. A real Apple peer may reject data
  frames with the wrong head magic. Discovery (action frames) was unaffected.

**NOT changed**: election `METRIC_INIT`/`COUNTER_INIT` stay at owl's canonical
60/0 (not the uncommitted election.h edit's 2000/2000000).

Coordinate with Claude (orchestrator) for: workspace dep additions, the awdl0
contract, and anything that would touch shared files. Keep all your code under
`crates/filin-rs/`.
