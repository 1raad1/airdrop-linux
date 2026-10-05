# filin-rs Operator Run Guide

`filin` is the AWDL link-layer daemon. It creates `awdl0`, binds a Wi-Fi
monitor interface for raw 802.11 RX/TX, and bridges Ethernet frames between
`awdl0` and AWDL data/action frames.

## Prerequisites

- Linux with `/dev/net/tun`, AF_PACKET, ioctl netdev controls, and nl80211.
- Run as root, or with the needed capabilities. In practice use root for the
  first hardware pass:
  - `CAP_NET_ADMIN` for TAP/netdev/nl80211 setup.
  - Raw packet socket permission, normally `CAP_NET_RAW`.
- A Wi-Fi interface already capable of monitor-mode injection/capture.
- Use plain monitor mode. Do not rely on active monitor mode.
- Known-good hardware from the C owl pass: Atheros `carl9170` USB, especially on
  5 GHz channel 44.

## Invocation

```bash
sudo filin -i <monitor_iface> -c 44 -h awdl0 -N
```

Flags:

- `-i <monitor_iface>`: Wi-Fi monitor interface used for raw 802.11 RX/TX.
- `-c 44`: anchor/social channel. Use `44` for modern Apple devices unless
  deliberately testing `6` or `149`.
- `-h awdl0`: host TAP interface name created for upper layers.
- `-N`: assume the Wi-Fi interface is already in monitor mode.

## Startup Behavior

At startup `filin`:

1. Opens `/dev/net/tun` and creates a TAP interface named by `-h`.
2. Reads the monitor interface MAC address and applies it to `awdl0`.
3. Sets `awdl0` MTU to `1450`.
4. Marks `awdl0` up.
5. Sets the monitor interface channel via nl80211 from `-c`.
6. Opens an AF_PACKET raw socket bound to the monitor interface.
7. Enters the RX/TX poll loop.

The poll loop:

- Parses radiotap, 802.11, AWDL PSF/MIF action frames, and AWDL TLVs.
- Uses radiotap TSFT as the RX timestamp source, bridged into monotonic time.
- Decapsulates AWDL data frames to Ethernet and writes them to `awdl0`.
- Reads Ethernet frames from `awdl0`, encapsulates them as AWDL data frames, and
  injects them through the monitor interface.

## Verifying Sync

Run with tracing enabled:

```bash
RUST_LOG=filin_rs=trace sudo filin -i <monitor_iface> -c 44 -h awdl0 -N
```

Look for the sync log line:

```text
sync_error error_tu=<...> aw_counter=<...> time_to_next_aw_tu=<...>
```

This value is computed from the TSFT-bridged RX timestamp, not host packet
arrival time. Stable low `error_tu` is the primary sign that sync is usable.

## Verifying awdl0 Traffic

In another terminal:

```bash
ip link show awdl0
sudo tcpdump -i awdl0 -n -vv
```

Expected signs:

- `awdl0` exists, has the monitor interface MAC, MTU `1450`, and is `UP`.
- `tcpdump` shows IPv6/mDNS or other Ethernet traffic once an Apple peer is
  active and sync is good.

You can also inspect the monitor side:

```bash
sudo tcpdump -i <monitor_iface> -I -n -e -vv
```

## Validation Status

This path is compile-tested and unit-tested, including the radiotap TSFT bridge,
AWDL action/data parsers, election/sync/channel logic, TAP setup planning, and
raw syscall wrappers. It has not yet been live-validated against a real Apple
device in this Rust port.
