# Source provenance and changes

This distribution includes the complete modified sources used by its binaries. It preserves existing copyright and license notices; no ownership of upstream work is claimed.

| Component | Upstream / revision | License |
| --- | --- | --- |
| OpenDrop Rust (`luftlift`, `filin`) | `ayourtch-llm/opendrop-rs`, `dccc798e244363eb92d35e3c52e9a913188dda91` | GPL-3.0-only |
| OWL compatibility backend | `jedbillyb/owl`, `832d70f815c3d4a06a02117bf0fc5e868daa1ff0` | GPL-3.0-or-later |
| radiotap library | `radiotap/radiotap-library`, `94984dd829a605a2d7af55241145c159607f3b30` | ISC-style license in source |
| GoogleTest (OWL build/tests) | `google/googletest`, `703bd9caab50b139428cea1aaff9974ebee5742e` | BSD-3-Clause |
| Vendored mdns-sd | Version in `native/opendrop-rs/vendor/mdns-sd/Cargo.toml` | MIT OR Apache-2.0 |

OWL/OpenDrop originated at SEEMOO Lab / the Open Wireless Link project. Protocol comparisons also used [bodaay/GoOpenDrop](https://github.com/bodaay/GoOpenDrop) and [jedbillyb/airdrop-mt7921](https://github.com/jedbillyb/airdrop-mt7921).

Local Rust changes include interface-scoped discovery/listening; mDNS hop limit 255, stable per-machine announcement identity, short TTL/refresh/goodbye; explicit desktop approval; safe duplicate saving; complete-archive checks; stored dvzip blocks; bounded stalled reads; and folder opening after acknowledgement. The Python sender implements the Ask/Upload sequence verified on the real iPhone, with disk-spooled archives, Bluetooth discovery, scoped TLS, progress, cancellation and per-socket BBR.

Local OWL changes include fixed-channel pin compatibility, duplicate suppression for already-delivered retry frames, preserved injection sequence/order and initialized radiotap metadata. The original `filin` backend remains available for other adapters but is not covered by the verified Realtek hardware result.

The desktop layer adds temporary privileged radio setup, regular-user file I/O, session cleanup/extension, optional Qt tray, optional Dolphin service menu, dynamic hostname/XDG paths and a portable installer. The initial working baseline is retained before later throughput experiments.

Cargo dependency versions are locked. Other dependencies retain their own licenses. Source and build instructions accompany release binaries; platform-specific binaries should not be mistaken for universal Linux support.
