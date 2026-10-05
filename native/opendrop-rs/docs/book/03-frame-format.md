# Chapter 3 — The Frame Format

This is the chapter where AWDL stops being a story and becomes bytes. Everything the protocol does — electing a master, aligning clocks, advertising channels, discovering services, moving data — is carried in just **two** frame types: the **action frame** and the **data frame**. Action frames carry control information as a bag of TLVs; data frames carry IPv6 packets. Understand these two structures and their TLV catalogue, and you can read an AWDL capture. This chapter is denser than its neighbors by design; treat it as a reference you'll come back to while reading Chapters 4 through 7.

## Vendor-specific frames: the escape hatch AWDL rides on

AWDL doesn't invent new 802.11 frame *types* — it couldn't, and still run on commodity chips. Instead it uses 802.11's built-in **vendor-specific** mechanism. The standard reserves a way for any organization holding an **Organizationally Unique Identifier (OUI)** to define frames with arbitrary vendor payloads. Apple uses this twice over: action frames use the vendor-specific **action** category, and data frames use vendor-specific **LLC/SNAP** encapsulation. Both are perfectly legal 802.11 that any chip will transmit; the magic is entirely in what Apple stuffs into the vendor payload.

A small but load-bearing constant: every AWDL frame uses the fixed **BSSID `00:25:00:ff:94:73`**. The OUI `00:25:00` is assigned to Apple. This BSSID is so distinctive that the simplest possible AWDL sniffer is just a BPF filter that forwards only frames bearing it — the OWL implementation does exactly that to pluck AWDL frames out of the air and drop everything else.

## The action frame

Action frames are the control plane. They have a **fixed-size header** followed by a **variable-length list of TLVs**. Schematically, from the start of the 802.11 frame:

```
802.11 MAC header
  Category (127 = vendor-specific)
  OUI (00:17:f2 — Apple)
  Type/Subtype (Action)
  Duration (0)
  Destination Address
  Source Address
  BSSID (00:25:00:ff:94:73)
  Sequence/Fragment number
--- AWDL fixed header ---
  Type (8)
  Version (e.g. 1.0)
  Subtype          <- PSF or MIF
  Reserved (0)
  PHY Tx time    T_Tx,PHY      (microseconds)
  Target Tx time T_Tx,Target   (microseconds)
--- payload ---
  TLVs ...
  (FCS at end of 802.11 frame)
```

A few fields earn special attention:

- **The two timestamps.** `T_Tx,Target` records when the frame's contents were *created* (so, when the timing information inside was current), and `T_Tx,PHY` records when the frame was *queued for transmission*. Their difference approximates the sender's transmission delay, and the receiver uses it to correct for that delay when synchronizing. This is the protocol's attempt to compensate for the gap between "I computed this timing value" and "I actually put it on the air." (Chapter 4 shows the exact equation, and why the macOS implementation's placement of these timestamps is the dominant source of synchronization error.)
- **Subtype** selects one of the two action-frame flavors.

### PSF vs MIF

There are two action-frame subtypes, and they share the identical fixed header — they differ only in subtype value and in *which TLVs they carry*:

- **Periodic Synchronization Frame (PSF)**, subtype **0**. Used for synchronization. If every device in the cluster supports 5 GHz, the PSF is the only frame type you'll see down on the 2.4 GHz band. PSFs are emitted by **all** nodes (not only the master), which — as Chapter 1 hinted and Chapter 5 expands — is one reason AWDL is less power-efficient than its design aspirations suggested.
- **Master Indication Frame (MIF)**, subtype **3**. The workhorse. It carries more TLVs and is broadcast by all devices regularly. It serves election *and* service discovery and generally carries the richer picture of a node's state.

A subtle timing wrinkle worth filing away: MIFs tend to be transmitted following the advertised channel sequence and clustered around the *middle* of an extended availability window (so that even imperfectly synchronized peers are likely to catch them), while PSFs are sprayed more uniformly in time, governed by a separate "AF period" (typically 110 or 440 TU) that does *not* line up with the 64-window channel sequence. The likely reason is bootstrapping: a steady background of PSFs at all times helps brand-new, not-yet-synchronized nodes discover an existing cluster fast.

## The TLV system

A **TLV** is the universal container for control information. Each one is:

- **Type** — 1 byte. Identifies what the value means.
- **Length** — 2 bytes. The length of the value field that follows, in bytes.
- **Value** — `Length` bytes of payload.

That 1-byte type, 2-byte length convention is worth memorizing, because it shows up in the exploit chapter: a parser walking a list of TLVs is repeatedly trusting a length field to tell it how far to read. If any handler trusts that length without bounds-checking against the actual frame size, you have a classic over-read or over-write primitive — which is precisely what happened with the Sync Tree TLV (Chapter 9).

### The TLV catalogue

Here is the working catalogue reconstructed by SEEMOO, grouped by purpose. The names come from function names and debug strings found in the driver binaries, so they reflect Apple's own internal vocabulary. A checkmark indicates the TLV appears in that frame subtype.

| TLV | Type | PSF | MIF | Purpose |
|---|---|:--:|:--:|---|
| Synchronization Parameters | 4 | ✓ | ✓ | Election & synchronization |
| Channel Sequence | 18 | ✓ | ✓ | Election & synchronization |
| Election Parameters | 5 | ✓ | ✓ | Election & synchronization |
| Election Parameters v2 | 24 | ✓ | ✓ | Election & synchronization |
| Synchronization Tree | 20 | ✓ | ✓ | Election & synchronization |
| Service Parameters | 6 | ✓ | ✓ | Service discovery |
| Service Response | 2 | | ✓ (may repeat) | Service discovery |
| Arpa (reverse DNS / hostname) | 16 | | ✓ | Service discovery |
| Data Path State | 12 | ✓ | ✓ | User-data transmission |
| HT Capabilities | 7 | | ✓ | User-data transmission |
| VHT Capabilities | 17 | | ✓ | User-data transmission |
| Version | 21 | ✓ | ✓ | Compatibility |

Some type values (notably 1, 3, and 8) are absent — they appear deprecated, unused in the AWDL versions analyzed. Note also a curious redundancy that recurs throughout AWDL: the **Synchronization Parameters TLV already contains its own channel sequence**, which makes the separate **Channel Sequence TLV** (type 18) look redundant — yet current implementations always send both. AWDL is full of this kind of belt-and-suspenders duplication; the current master MAC address, for instance, shows up in *three* different TLVs (Sync Parameters, Election Parameters, and Election Parameters v2), and when AirDrop is active the service instance string and device name can appear three times in a single frame.

Let me expand the TLVs that the next several chapters depend on.

### Synchronization Parameters TLV (type 4)

This is the most important single structure in the protocol — it drives both timing and channel hopping. Its fields (lengths vary; this is the logical layout) include:

- **TX Channel** and **Master Channel** — the node's current transmit channel and the master's home channel.
- **Tx Counter (`t_AW`)** — the number of TUs until the next extended availability window begins. This is the live countdown a receiver uses to find the next window boundary.
- **AW Period (16)**, **AF Period (110 or 440)** — window length and action-frame cadence.
- **Flags**, **AW Extension Length (16)**, **AW Common Length (16)**, **Remaining AW**.
- **Extension counts**: `Ext Min (3)`, `Multicast Max (3)`, `Unicast Max (3)`, `AF Max (3)` — these govern extension windows (Chapter 4). In practice all fixed at 3.
- **Master MAC Address**, **Presence Mode (4)**.
- **Sequence Number `i`** — the index of the current availability window.
- **AP Beacon Alignment**, and an embedded **Channel Sequence**.

Almost every "static value" mentioned in Chapter 4 — the 16-TU window, the presence mode of 4 — is literally a field in this TLV that *could* be configured differently but never is in shipping software.

### Channel Sequence TLV (type 18)

Encodes the channel-hopping pattern: a count `c` (15, giving `c+1 = 16` entries), an **encoding**, a **duplicate count**, a **step** field, a **fill channel** (`0xffff`), and the **channel list** itself. Chapter 6 derives the exact formula mapping a window sequence number to the active channel. The headline: with Apple's fixed `c = 15` and `step = 3`, the sequence covers `(15+1)·(3+1) = 64` windows ≈ 1 second.

### Synchronization Tree TLV (type 20)

A list of node addresses tracing the path from the announcing node up to the **top master**. Its job is loop prevention and bounding the election tree depth (Chapter 5). Its job in the *security* story is more notorious: it's a variable-length list whose handling contained the out-of-bounds bug behind CVE-2020-3843 (Chapter 9). When you read "a variable-length list of addresses parsed in the kernel," you should already be a little nervous.

### Election Parameters / v2 (types 5, 24)

Carry the **master metric** that decides who wins the election, plus the current master's address and assorted counters. Version 2 is the one whose metric field actually governs election in the analyzed versions. Chapter 5 covers why that metric is, surprisingly, essentially random.

### Service Parameters (type 6), Service Response (type 2), Arpa (type 16)

The service-discovery payload. These offload **mDNS / DNS-SD** into the action frame: the Arpa TLV carries the hostname (reverse-DNS style), and the Service Response TLV carries the PTR, SRV, and TXT resource records — i.e. the Bonjour advertisement. Service Response may appear multiple times in one MIF. Chapter 7 unpacks the encoding.

### Data Path State (type 12), HT/VHT Capabilities (types 7, 17)

These negotiate the actual data connection. **Data Path State** announces, among other things, the infrastructure **BSSID** the node is currently associated to *and the real (non-randomized) MAC address of the Wi-Fi chip* — a detail with privacy implications, since it can tie a randomized AWDL identity back to a hardware address. HT/VHT Capabilities advertise supported PHY rates, mirroring the 802.11n/ac capability elements.

### Version TLV (type 21)

A compact version stamp: roughly half a byte each for major and minor version, plus a **device class** byte that hints at the OS type (e.g. macOS vs iOS). Observed mappings: AWDL v3.x in macOS 10.13 / iOS 11, v2.x in macOS 10.12 / iOS 10, v1.x in macOS 10.11 (which predates the Version TLV). The version also feeds election: the random-metric *range* depends on the version, engineered so that newer-version nodes tend to win and become master (Chapter 5).

## The data frame

Data frames are mercifully simpler. They are 802.11 **data** frames with To-DS and From-DS both zero (so they're directly addressed, three-address, much like IBSS), carrying an LLC/SNAP header and then a tiny AWDL-specific data header:

```
802.11 MAC header (data, To-DS=0, From-DS=0)
  Destination / Source / BSSID (00:25:00:ff:94:73)
--- LLC/SNAP ---
  DSAP 0xAA  SSAP 0xAA  Control 0x03
  OUI 00:17:f2 (Apple)
  Protocol ID
--- AWDL data header ---
  Magic bytes (0x0304)
  Sequence number
  Reserved (0)
  EtherType (0x86dd = IPv6)
--- payload ---
  IPv6 packet
```

The whole AWDL data header amounts to a **sequence number** and an **EtherType** — and the only EtherType ever observed is `0x86dd`, IPv6. So a data frame is, in essence: "here is an IPv6 packet, numbered, from this source MAC, on the AWDL BSSID." When the OWL daemon receives one, it strips the AWDL data header, slaps on a normal Ethernet header, and hands it up to the virtual `awdl0` interface; outbound, it does the reverse and assigns the sequence number from an internal counter. That symmetry is exactly how `awdl0` manages to look like an ordinary IPv6 interface to the rest of the stack.

## Reading a frame in practice

Put it together and the decode procedure is mechanical:

1. Filter on BSSID `00:25:00:ff:94:73`.
2. Is it action or data? Action → control; data → strip header, it's an IPv6 packet.
3. For an action frame, read the fixed header (note the two timestamps and the subtype: PSF or MIF).
4. Walk the TLV list: read 1-byte type, 2-byte length, then `length` bytes of value; dispatch on type using the catalogue above; repeat until the frame ends.

SEEMOO published a Wireshark dissector that does precisely this, and the OWL source implements the same walk in C. With the catalogue in hand, you now have the map you need for the next four chapters, each of which takes one cluster of these TLVs and explains the machinery it drives — starting, in Chapter 4, with the Synchronization Parameters TLV and the structure of time itself.

---

*Next: Chapter 4 — how AWDL carves time into availability windows, how the master clock is distributed, the synchronization equation, and why ~10% of every window is sacrificed to a guard interval.*
