# Chapter 10 — Build It Yourself, and the References Behind This Book

The best way to consolidate everything in the previous nine chapters is to *see it on the wire* — to watch real availability windows, real master election, real TLVs scroll past in a packet capture, and ideally to run an implementation that participates in an AWDL cluster from a non-Apple device. This final chapter is a practical on-ramp: the open-source tooling that exists, how to approach capturing and dissecting AWDL traffic, ideas for experiments that map onto specific chapters, and then the consolidated reference list that this whole book is built on.

A caveat up front, in keeping with the book's honesty about uncertainty: tooling, OS internals, and driver behavior **drift over time**. AWDL has evolved across OS versions (the v1/v2/v3 progression from Chapter 3 continued), Apple has hardened the driver since the exploits of Chapter 9, and the open-source projects below were last substantially active a few years ago. Treat specific commands and version numbers as starting points to verify, not gospel — check each project's current README before relying on it.

## OWL: the open AWDL implementation

The single most valuable artifact for hands-on learning is **OWL (Open Wireless Link)** — a clean, user-space implementation of AWDL in C, from the same SEEMOO group whose research underpins this book. OWL lets a Linux machine with a suitable Wi-Fi card actually **participate** in an AWDL cluster: take part in election and synchronization, be discovered by Apple devices, and send/receive AWDL data frames.

Reading the OWL source is, frankly, the fastest path to grounding the abstractions in this book. Concrete mappings:

- `src/rx.c` — the receive path: how frames are parsed, how TLVs are walked and dispatched, how the sync-params and channel-sequence TLVs are handled. This is Chapter 3, 4, and 6 made executable. (It's also where you can see the very TLV-walking-with-trusted-lengths pattern that Chapter 9 warned about — including a TODO about mitigating a desynchronization attack by checking the destination address.)
- The transmit path — how frames are constructed with the right fixed header and TLVs before going out the Wi-Fi interface.
- The election and synchronization logic — Chapters 4 and 5, as real state machines.
- The data-frame handling — stripping the AWDL data header and forwarding IPv6 to a virtual `awdl0` interface, and the inverse for outbound (Chapter 3's data frame, Chapter 7's addressing).

How OWL works operationally mirrors Chapter 3's "reading a frame" procedure exactly: it puts a card in monitor mode, filters frames by the AWDL BSSID `00:25:00:ff:94:73` with a BPF filter, runs election/synchronization on received action frames, derives each peer's link-local IPv6 from its source MAC (the RFC 4291 EUI-64 construction from Chapter 7), and bridges data frames to a virtual interface. Requirements are non-trivial — it needs a Wi-Fi card and driver combination that supports **active monitor mode with frame injection**, which historically meant specific Atheros (`ath9k`-class) hardware. Check the project's current hardware-compatibility notes.

A companion project, **OWL's userland tooling and the related `owlink.org` site**, collects the group's AWDL/AirDrop work in one place and is the best jumping-off point.

## The Wireshark dissector

For pure observation (no participation), SEEMOO published an **AWDL dissector for Wireshark**. With it, every field this book described — the fixed header, the two synchronization timestamps, each TLV type, the channel sequence, the sync tree, the service-discovery records — is decoded and labeled in the Wireshark UI. Modern Wireshark versions have included AWDL dissection support derived from this work, so depending on your version you may be able to dissect AWDL frames out of the box.

The capture workflow, conceptually:

1. Put a capable Wi-Fi card into **monitor mode**.
2. Because AWDL hops between social channels (6, 44, and possibly 149) and data channels, a single monitor card parked on one channel sees only part of the picture. SEEMOO's research rig used **two** monitoring cards tuned to the primary (44) and secondary (6) channels simultaneously, with hardware timestamping, then aligned the two captures in post-processing using a calibration phase. For casual observation, parking on channel 44 (the primary) captures the bulk of coordination traffic; for serious timing work you'll want the dual-card approach.
3. Filter on the AWDL BSSID and let the dissector decode.

## Tooling on Apple devices themselves

If you have a Mac and want to observe AWDL from the inside, the research describes several built-in facilities (subject to SIP restrictions and OS changes):

- **The `awdl0` interface** is visible to `ifconfig` and friends; you can watch it come up on demand (e.g. when you open the AirDrop share sheet) and go away after.
- **Verbose driver logging** via boot-args (SEEMOO used `awdl_log_flags` and related `nvram boot-args` settings) surfaces state transitions, the current channel sequence, and master/sync details in the Console — e.g. log lines showing the AWDL on-state, the infra percentage, and the channel list like `(6/44/44) [44 0 0 0 0 0 0 0 6 44 44 0 0 0 0 0]`. Enabling these requires disabling **System Integrity Protection**, which has real security implications — only do this on a test machine you understand.
- **CoreCapture**, Apple's 802.11 tracing framework, produces PCAP traces in a custom format (SEEMOO wrote a dissector for it).

These are powerful but invasive and version-sensitive; the boot-args and entitlement checks have changed over OS releases (recall from Chapter 1 that Apple closed the unrestricted `ioctl` access SEEMOO originally used, behind an entitlement, after it was reported as CVE-2017-13886).

## Experiments that map to chapters

If you get a capture rig working, here are experiments that make specific chapters tangible:

- **Election (Ch. 5):** Activate AWDL on several Apple devices a few seconds apart (open AirDrop on each). Watch the advertised **self metric** and **current master** fields. You should see the newest-version, highest-random-metric device win — and possibly watch a sitting master get "overtaken" by chance, exactly as Chapter 5 described.
- **Synchronization accuracy (Ch. 4):** With dual hardware-timestamped captures, compute the synchronization error between a slave and its master using the equation from Chapter 4. You should find a roughly Gaussian distribution with sub-TU mean and the 3-TU target met >99% of the time.
- **Channel allocation states (Ch. 6):** Capture during an idle interface vs. during a large AirDrop transfer and compare the advertised channel sequences. Watch AWDL escalate from the ~25% "low power" allocation toward the 100% "data" state as load increases, and note the mandatory channel-6 slot persisting throughout.
- **Service discovery (Ch. 7):** Decode the Service Response and Arpa TLVs in MIFs and watch the Bonjour PTR/SRV/TXT records and hostname appear — often redundantly several times per frame.

## A responsible-use note

Chapters 8 and 9 covered real privacy and security weaknesses. Capturing and dissecting AWDL traffic for learning, research, and security testing on devices you own or are authorized to test is exactly the kind of independent audit the research community argues *should* be possible for a protocol on a billion-plus devices. Using these techniques to harvest strangers' contact identifiers, track people, or attack devices you don't own is another matter entirely — both unethical and, in most jurisdictions, illegal. Keep your experiments to your own devices and a Faraday-bag-or-quiet-RF-environment mindset, as the researchers did (they worked inside a Faraday tent to avoid interference and avoid capturing bystanders).

---

## References

The backbone of this book is the SEEMOO (Secure Mobile Networking Lab, TU Darmstadt) body of work, the OWL implementation, and Ian Beer's Project Zero analysis.

**Core protocol specification (reverse-engineered):**

- Milan Stute, David Kreitschmann, Matthias Hollick. **"One Billion Apples' Secret Sauce: Recipe for the Apple Wireless Direct Link Ad hoc Protocol."** MobiCom 2018. — The foundational frame-format and operation reconstruction; nearly every structural detail in Chapters 3–7 traces here. (arXiv: 1808.03156)
- Stute et al. **"Demo: Linux Goes Apple Picking: Cross-Platform Ad hoc Communication with Apple Wireless Direct Link."** (arXiv: 1812.06743) — The OWL demo; the receive/transmit pipeline and BSSID-filter approach.

**Security and privacy:**

- Milan Stute, Sashank Narain, Alex Mariotto, Alexander Heinrich, David Kreitschmann, Guevara Noubir, Matthias Hollick. **"A Billion Open Interfaces for Eve and Mallory: MitM, DoS, and Tracking Attacks on iOS and macOS Through Apple Wireless Direct Link."** USENIX Security 2019. — The systematic link-layer attack analysis behind Chapter 9's MitM/DoS/tracking discussion.
- Alexander Heinrich, Matthias Hollick, Thomas Schneider, Milan Stute, Christian Weinert. **"PrivateDrop: Practical Privacy-Preserving Authentication for Apple AirDrop."** USENIX Security 2021. — The contact-identifier leak and the PSI-based fix; the heart of Chapter 8's privacy section. (Project page: privatedrop.github.io)
- Heinrich et al. **"AirCollect: Efficiently Recovering Hashed Phone Numbers Leaked via Apple AirDrop."** WiSec 2021. — The rainbow-table phone-number-hash reversal (milliseconds), demonstrating why the hash exchange leaks.
- Ian Beer. **"An iOS zero-click radio proximity exploit odyssey."** Google Project Zero, 2020. — The landmark zero-click AWDL RCE; the SyncTree TLV out-of-bounds write (CVE-2020-3843) and the full exploit chain in Chapter 9.

**Implementations and tooling:**

- **OWL (Open Wireless Link)** — `github.com/seemoo-lab/owl` and `owlink.org`. The open AWDL implementation; `src/rx.c` and the election/sync/data-path code referenced throughout.
- **SEEMOO AWDL Wireshark dissector** — bundled with the MobiCom work; AWDL dissection has since been upstreamed into Wireshark.
- **OpenDrop** — `github.com/seemoo-lab/opendrop`, an open AirDrop implementation (Python), useful for the Chapter 8 application layer.
- Community frame parsers, e.g. `Frostie314159/awdl-frame-parser` (Rust; designed to run even in constrained environments like ESP32/wasm) — a readable, modern take on the frame format from Chapter 3, and a good companion if you're working in Rust.

**Apple-side primitives (for context):**

- RFC 4291, *IP Version 6 Addressing Architecture* — the EUI-64 link-local derivation used for AWDL addressing (Chapter 7).
- Apple's iOS/macOS security documentation — for the current (post-research) state of AirDrop privacy defaults and Wi-Fi driver hardening. Worth consulting fresh, since this area keeps changing.

---

*That closes the field guide. You started with a single hardware constraint — one radio, one channel at a time — and followed it through synchronized time, random-metric elections, self-describing channel schedules, Bonjour folded into beacons, IPv6 conjured from MAC addresses, AirDrop's TLS-over-AWDL exchange, and the security and privacy consequences of an open, kernel-parsed, broadcast-everything protocol on a billion devices. The most valuable habit to carry forward is the one this book tried to model: when a system is undocumented, the truth lives in the reverse-engineering papers, the open reimplementations, and the exploit writeups — read those, hold their uncertainty honestly, and verify against the wire.*
