# opendrop-rs — AWDL + AirDrop for Linux, in Rust

A Rust implementation of Apple Wireless Direct Link (AWDL) and AirDrop for Linux,
**derived from** the SEEMOO Lab [owl](https://github.com/seemoo-lab/owl) and
[opendrop](https://github.com/seemoo-lab/opendrop) projects (the AWDL layer was
written by studying and translating owl). Because it is a derivative of those
GPLv3 works, **this project is licensed under the GNU GPL v3.0** (see
[`LICENSE`](LICENSE)).

The workspace is split into two independent binaries:

| Crate                | Binary     | Role |
|----------------------|------------|------|
| `crates/filin-rs`    | `filin`    | AWDL link layer — creates `awdl0`, drives a Wi-Fi monitor interface, runs election/sync, bridges Ethernet ↔ AWDL frames. |
| `crates/luftlift-rs` | `luftlift` | AirDrop application — mDNS discovery, TLS/HTTPS, the `/Discover` `/Ask` `/Upload` protocol; **send**, **receive**, **find**. |

## Status

**Validated end-to-end against real Apple devices** — files have been received from
an iPhone (iOS 18) to Linux over AWDL via this stack. The pure-software layers are
unit-tested and clippy-clean:

- `cargo build --release` — both binaries build clean.
- `cargo test --workspace` — 360+ tests passing.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.

Known limits (hardware/protocol, not bugs — see the design notes in `docs/dev/`):

- **Sending to an idle iPhone isn't supported** — AirDrop discovery is triggered by
  a BLE advertisement we don't emit; the supported direction is *Apple → luftlift*.
- **Shared web links don't transfer** to an unauthenticated receiver — iOS withholds
  the payload without an Apple-signed identity record. Photos/files/notes work.
- **Very large uploads** can stall at the endgame on a single half-duplex monitor
  radio (no MAC-layer retransmission on injected frames).

## Architecture

```
       Wi-Fi monitor iface (raw 802.11, e.g. carl9170 @ ch44)
                         │
                   ┌─────┴─────┐
                   │  filin    │   radiotap → 802.11 → AWDL action/data frames
                   │ (AWDL)    │   election · sync · TSFT bridge · channel · TX
                   └─────┬─────┘
                         │  creates + bridges
                      awdl0 (TAP, MTU 1450)
                         │
                   ┌─────┴─────┐
                   │ luftlift  │   mDNS · TLS · HTTP · dvzip/CPIO · Discover/Ask/Upload
                   │ (AirDrop) │   send · receive · find
                   └───────────┘
```

`filin` brings up the AWDL link and exposes it as an ordinary `awdl0` network
interface; `luftlift` then speaks AirDrop over `awdl0` like any other IPv6
link-local service. The two are decoupled — `luftlift find` works on a normal LAN
without `filin` running.

Design notes worth knowing:

- **AWDL sync uses the radiotap TSFT hardware timestamp**, bridged into monotonic
  time (offset filtered with clamping), *not* host packet-arrival time. This is the
  single most important correctness point for sync; see `docs/awdl-sync.md`.
- **AirDrop bodies are `application/x-dvzip`**: framed `[u32 BE len][zlib]` blocks
  wrapping an ODC CPIO archive (magic `070707`). POSTs are chunked (no
  `Content-Length`). See `docs/airdrop-protocol.md`.

## Prerequisites

- **Linux** (uses `AF_PACKET` monitor sockets and TAP interfaces).
- A recent **stable Rust** toolchain (`rustup`); edition 2021. No system libraries
  to install — the only native dependency is `libc`, and TLS is pure-Rust `rustls`.
- **`filin` needs root** (or `CAP_NET_RAW`+`CAP_NET_ADMIN`) and a Wi-Fi adapter that
  supports **monitor mode with frame injection** — see [Hardware](#hardware).
- `luftlift` alone (`find`, or `receive`/`send` over a normal LAN) needs no root and
  no special hardware.

## Build

```bash
cargo build --release          # -> target/release/{filin,luftlift}
cargo test --workspace
```

## Usage

### luftlift (AirDrop) — works on a normal network without filin

```bash
luftlift find -d 5                              # discover AirDrop receivers
luftlift receive -o ./incoming                 # receive (mDNS announce + HTTPS)
luftlift send "<recipient-instance-name>" file # send to a discovered receiver
```

Set `RUST_LOG=luftlift_rs=debug` for detail. The receiver tolerates the awdl0
interface coming and going (it re-announces when the link returns).

### filin (AWDL) — requires root + a Wi-Fi monitor-capable adapter

```bash
sudo ./target/release/filin -i <wifi_iface> -c 44 -h awdl0
```

`filin` self-configures the radio by default: it releases the interface from
NetworkManager, sets monitor mode, brings it up, and tunes the channel — and it
**auto-recovers** if the card is flapped, unplugged/replugged, or grabbed back by
NetworkManager (it reopens within ~1s). Key flags:

| Flag | Meaning |
|------|---------|
| `-i <iface>` | Wi-Fi interface to use as the raw 802.11 monitor |
| `-c 44` | anchor/social channel (44 for modern Apple; also 6, 149) |
| `-h awdl0` | name of the TAP interface to create |
| `--check` | preflight only: probe the `-i` adapter (does its driver support monitor mode?), print a verdict, and exit. Needs no root. |
| `-N` / `--assume-monitor` | skip monitor-mode config (something else owns the radio's mode) |
| `--force-master` / `--no-force-master` | win the AWDL election so peers follow filin's channel (on by default) |
| `--tx-retransmits N` | re-inject each unicast data frame N extra times (default 0; see notes) |

Full operator guide: **[`crates/filin-rs/RUN.md`](crates/filin-rs/RUN.md)**.

### End-to-end (Apple → Linux)

```bash
# terminal 1 — AWDL link up (self-configures the monitor)
sudo ./target/release/filin -i <wifi_iface> -c 44 -h awdl0
# terminal 2 — AirDrop receiver on the awdl0 link
./target/release/luftlift -i awdl0 receive -o ./incoming
```

Then, on an Apple device with the Sharing pane open and AirDrop set to *Everyone*,
send a photo/file to the Linux host — it should appear in the picker and the file
lands in `./incoming`.

## Hardware

AWDL sync over USB monitor interfaces is the delicate part. Not sure if an
adapter will work? Probe it first (no root needed):

```bash
filin -i <wifi_iface> --check
```

This reports whether the driver supports monitor mode and exits non-zero if the
adapter is unusable. Note it can only check *capabilities* — some adapters
(e.g. Realtek `rtw88`) report monitor mode but still silently drop injected
**data** frames, which `--check` warns about but cannot detect.

- **Known-good injector:** Atheros `carl9170` USB in plain monitor mode on ch44 —
  it injects both AWDL action *and* data frames and time-syncs well.
- **Realtek `rtw88` (8822bu):** injects management/action frames only — it does
  **not** transmit data frames in monitor mode, so it can't carry AirDrop traffic.
- Use **plain** monitor mode; do not rely on active monitor mode.

More detail in `docs/dev/` (radio comparison, sync, channel-switch latency).

## Reference: SEEMOO `owl`

The original GPLv3 `owl` is not vendored. To check the port
against it, fetch it on demand into a git-ignored `./owl/`:

```bash
scripts/fetch-owl.sh
```

## Repository layout

```
crates/filin-rs/        AWDL link layer (bin: filin)  + RUN.md, PORT.md
crates/luftlift-rs/     AirDrop app    (bin: luftlift) + PORT.md
docs/ARCHITECTURE.md    high-level design
docs/awdl-sync.md       the TSFT-timestamp sync lesson
docs/airdrop-protocol.md reverse-engineered iOS AirDrop wire details
docs/book/              long-form write-up of AWDL + AirDrop
docs/dev/               development notes / debugging history
scripts/fetch-owl.sh    fetch the GPLv3 owl reference (not bundled)
```

## Credits & license

Derived from the SEEMOO Lab `owl` (AWDL) and `opendrop` (AirDrop) projects — the
AWDL link layer here was written by studying and translating `owl`. All credit to
the SEEMOO Lab and the AWDL/AirDrop reverse-engineering literature.

Because this is a derivative of those GPLv3 works, **this project is licensed
under the GNU General Public License v3.0** — see [`LICENSE`](LICENSE).
