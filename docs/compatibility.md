# Compatibility and performance

[Back to README](../README.md)

## Tested setup

| Environment | Result |
| --- | --- |
| CachyOS, KDE Plasma/Wayland, RTL8852CE (`rtw89_8852ce`), kernel 7.2.9 | Tested with the existing internal Wi-Fi adapter and Ethernet internet |
| iPhone 13 Pro, iOS 27.0 | Received and sent files through native AirDrop; user confirmed files arrived and opened |
| iPhone → Linux, same 833,293-byte JPEG | 15.65 seconds, exact SHA-256 match; earlier version took 79.46 seconds |
| Linux → iPhone, 53-byte text | Delivered and opened |
| Linux → iPhone, 2,048,292-byte TIFF | Delivered and opened twice; uploads reduced from 233 seconds to about 14 seconds each using the temporary AP context; 1,224,353 bytes after lossless compression |
| Other adapters, distributions, desktop environments and iOS versions | Not verified on real hardware |

**Speed depends strongly on the Wi-Fi driver.** The tested Realtek adapter takes about 120 ms to change channels, so the compatibility backend stays on channel 44 while the iPhone hops. Plain monitor mode took 233 seconds to upload the TIFF, with about 19% retransmitted TCP bytes. Keeping a temporary AP hardware context on the same channel reduced the upload to about 14 seconds and retransmissions to about 1%. This is a hardware-specific workaround, and remains slower than typical Apple-to-Apple transfers. Other hardware is unverified.

## How the radio works

AirDrop uses Wi-Fi AWDL for file data. Bluetooth advertises discovery information; it does not carry the files. The selected Wi-Fi adapter must be disconnected from its normal network during an AirDrop session.

On `rtw89_8852ce`, automatic OWL/pin mode also starts a hidden WPA2 AP context on channel 44 using `wpa_supplicant`. It uses a fresh random key in a root-private `/run` directory and configures no DHCP, bridging or routing. File traffic continues over AWDL. This keeps the adapter's hardware MAC active; the observed speed gain suggests the monitor-only hardware state was a major bottleneck. Shutdown stops the context, removes its credentials and restores managed Wi-Fi mode. If setup fails, the receiver falls back to plain monitor mode. Other drivers do not automatically enable this workaround.

## Connection and consent

The sender reports success only after the phone acknowledges the upload. Connection establishment retries briefly; it never automatically repeats an acceptance request or an upload.

Temporary firewall exceptions permit IPv6 link-local TCP 8771 and UDP 5353 only on `awdl0`. Saved firewall configuration is not modified. AirDrop interoperability uses self-signed peer TLS identities; it does not authenticate a peer using a public CA or an Apple account. The launcher's explicit consent remains necessary.
