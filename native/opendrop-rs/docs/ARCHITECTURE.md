# opendrop-rs — a Rust port of owl (AWDL) + opendrop (AirDrop)

A Rust port of Apple's AirDrop stack for Linux, derived from the SEEMOO GPLv3
projects (it is **not** clean-room — the layers were written by studying and
translating their source):
- **owl** (C, SEEMOO) — the AWDL link layer → crate `filin-rs`
- **opendrop** (Python, SEEMOO) — the AirDrop application layer → crate `luftlift-rs`

We have working references (`owl/`, `opendrop-patch/`) and hard-won knowledge from
getting a real iPhone 15 Pro Max (iOS 18.6.2) to AirDrop a photo to Linux. The
goals of the Rust port: memory-safe, **far more debuggable/observable**, and
maintainable than the C/Python originals.

## The key architectural property: loose coupling

owl and opendrop do NOT call each other. owl creates a network interface
`awdl0` (an IPv6 link-local capable tun/tap); opendrop just opens normal
sockets bound to `awdl0`. **The two Rust crates mirror this**: they communicate
through the OS network interface, not a shared Rust API. This lets them be
developed and tested independently.

```
  ┌────────────────────────────────────────────────┐
  │  luftlift-rs   (mDNS, rustls HTTPS, Discover/Ask/ │   ← GLM-5.2
  │               Upload, plist, dvzip, cpio)        │
  └───────────────────────┬────────────────────────-┘
                          │ normal sockets bound to awdl0 (UDP 5353 mDNS, TCP 8771 https)
  ┌───────────────────────┴────────────────────────-┐
  │  awdl0  (IPv6 link-local netdev created by awdl) │
  └───────────────────────┬────────────────────────-┘
                          │ tun/tap + raw 802.11 inject/capture (pcap/nl80211)
  ┌───────────────────────┴─────────────────────────┐
  │  filin-rs  (radiotap, AWDL frames, election,      │   ← gpt-5.5
  │           sync, channel hop, monitor iface)      │
  └──────────────────────────────────────────────────┘
                          │
                  Wi-Fi monitor interface (Atheros carl9170 recommended)
```

## Crates (Cargo workspace)
- `crates/filin-rs/`   — AWDL daemon. Binary `filin`. Spec: `crates/filin-rs/PORT.md`.
- `crates/luftlift-rs/` — AirDrop CLI. Binary `luftlift` (send/receive/find). Spec: `crates/luftlift-rs/PORT.md`.

## Hard-won lessons to bake in (do NOT relearn these)
1. **Sync uses the hardware TSF, not the host clock.** owl's bug was timestamping
   RX frames with `clock_gettime` after USB delivery jitter. Use the radiotap
   **TSFT** field (hardware RX timestamp) bridged into a monotonic base. This is
   THE thing that makes sync tight enough for discovery. See `docs/awdl-sync.md`.
2. **Monitor mode / injection**: many drivers reject the `NL80211_MNTR_FLAG_ACTIVE`
   monitor flag. Set plain monitor mode externally and skip the driver's own
   monitor setup (owl's `-N`). Atheros `carl9170` gives clean sync (~0.4% error);
   Realtek `rtw88` clones are marginal and crash under injection.
3. **Channel**: default to **channel 44** (`-c 44`); modern iPhones run AWDL on 5GHz.
4. **TX backpressure**: inject can EAGAIN under load; size the send buffer and retry/queue.
5. **iOS 18 AirDrop quirks** (see `docs/airdrop-protocol.md`):
   - `/Discover` and `/Ask` POSTs use **chunked** transfer-encoding, **no Content-Length**.
   - File payload Content-Type is **`application/x-dvzip`** (older: `application/x-cpio`).
   - **dvzip format**: sequence of `[u32 big-endian length][zlib stream]` frames →
     concatenated inflate → an **ODC CPIO** (magic `070707`). Decoder: `opendrop-patch/dvzip.py`.
   - Links are shared as a `.webloc` file inside the CPIO (no special URL field).

## Debuggability (a core goal, not an afterthought)
Use `tracing` everywhere with structured fields. Emit events for: peer
add/remove, election master changes (with metric), per-window sync error,
channel switches, mDNS discover/announce, and each HTTP request phase. A `--json`
log mode and a live status surface (peers, current master, sync%, discovered
AirDrop services) should make the "why isn't it discovering" question trivial to
answer — the #1 pain in the C/Python stack.

## References in this repo
- `owl/` — C AWDL daemon (and our `src/rx.c` hardware-TSF patch)
- `opendrop-patch/` — patched Python opendrop (iOS-18 fixes) + `dvzip.py`
