# AirDrop Linux

Experimental native AirDrop sending and receiving for Linux, with optional Qt tray controls and KDE Dolphin integration. An iPhone can discover the computer in its normal AirDrop picker. The computer can send files to an iPhone through its normal acceptance prompt.

This is a community interoperability project based on [OpenDrop Rust](https://github.com/ayourtch-llm/opendrop-rs), [OWL](https://github.com/jedbillyb/owl) and the original [SEEMOO OpenDrop](https://github.com/seemoo-lab/opendrop)/[OWL](https://github.com/seemoo-lab/owl) projects. It is not an Apple or Google product.

## What is verified

| Environment | Result |
| --- | --- |
| CachyOS, KDE Plasma/Wayland, RTL8852CE (`rtw89_8852ce`), kernel 7.2.9 | Tested with the existing internal Wi-Fi adapter and Ethernet internet |
| iPhone 13 Pro, iOS 27.0 | Received and sent files through native AirDrop; user confirmed files arrived and opened |
| iPhone → Linux, same 833,293-byte JPEG | 15.65 seconds, exact SHA-256 match; earlier version took 79.46 seconds |
| Linux → iPhone, 53-byte text | Delivered and opened |
| Linux → iPhone, 2,048,292-byte TIFF | Delivered and opened twice; uploads reduced from 233 seconds to about 14 seconds each using the temporary AP context; 1,224,353 bytes after lossless compression |
| Other adapters, distributions, desktop environments and iOS versions | Not verified on real hardware |

**Speed depends strongly on the Wi-Fi driver.** The tested Realtek adapter takes about 120 ms to change channels, so the compatibility backend stays on channel 44 while the iPhone hops. Plain monitor mode took 233 seconds to upload the TIFF, with about 19% retransmitted TCP bytes. Keeping a temporary AP hardware context on the same channel reduced the upload to about 14 seconds and retransmissions to about 1%. This is a hardware-specific workaround, and remains slower than typical Apple-to-Apple transfers. Other hardware is unverified.

## Requirements

- Linux Wi-Fi driver with working monitor mode **and frame injection**. Reporting monitor support alone is insufficient. A radio already connected to Wi-Fi cannot be used; use Ethernet or a separate radio for internet access.
- NetworkManager, `iw`, `iproute2`, `ip6tables`, `pkexec` and a working polkit authentication agent. Administrator authentication creates temporary monitor/TAP interfaces; receiving and saving run as the regular user.
- Python 3.10+, OpenSSL, BlueZ; Python `dbus-python` and PyGObject for the Bluetooth discovery helper.
- PySide6 for the sending window and tray. Receiver consent uses `kdialog`, `zenity`, PySide6, or an interactive terminal. It declines if no consent interface is available.
- `xdg-utils`; optionally `notify-send`. Dolphin is used on KDE when available; other desktops use `xdg-open` after reception.
- To build: Rust/Cargo, C/C++ compiler, CMake, libpcap, libev and libnl development packages.
- `wpa_supplicant` for the RTL8852CE speed workaround; without it the application falls back to plain monitor mode.

Arch/CachyOS example (review the packages for your own setup):

```sh
sudo pacman -S --needed base-devel rust cmake libpcap libev libnl python python-pyside6 python-dbus python-gobject bluez iw iproute2 iptables networkmanager polkit openssl xdg-utils libnotify wpa_supplicant
```

Debian/Ubuntu build prerequisites:

```sh
sudo apt install build-essential cmake cargo libpcap-dev libev-dev libnl-3-dev libnl-genl-3-dev libnl-route-3-dev python3 python3-venv python3-dbus python3-gi bluez iw iproute2 iptables network-manager policykit-1 openssl xdg-utils libnotify-bin zenity wpasupplicant
```

Use a recent stable Rust toolchain if your distribution's Cargo cannot build the locked dependencies. If PySide6 is absent from the distribution, create a virtual environment that can access the system's D-Bus/GObject bindings:

```sh
python3 -m venv --system-site-packages .venv
.venv/bin/python -m pip install PySide6
```

Use that interpreter for installation; the generated desktop entries retain it. Keep its virtual environment in place.

## Build and install

```sh
git clone https://github.com/1raad1/airdrop-linux.git
cd airdrop-linux
./build.sh
python3 install.py
```

The installer runs as your regular user and copies the application into `${XDG_DATA_HOME:-~/.local/share}/airdrop-linux`. It generates launchers with the actual installation path and Python interpreter. It does not install dependencies or a permanent privileged service.

Options:

```sh
# KDE Dolphin context menu and tray at login (radio remains off at login)
python3 install.py --kde --autostart

# Core scripts only, without application-menu entries
python3 install.py --no-desktop

# A different installation directory
python3 install.py --prefix /path/to/airdrop-linux-install
```

The Qt tray can work on other desktops that support system tray icons, but only Plasma has been verified. The Dolphin service menu is specific to KDE. On other desktops use the sending application or command line.

Prebuilt release assets, if present, are labelled with their build platform. The initial tested binaries are x86-64 CachyOS/Arch builds requiring glibc 2.39 or newer and the listed runtime libraries, not universal Linux binaries. Build from source if your architecture or shared libraries differ.

## Receive from an iPhone

1. Start **AirDrop Receive**, or click **AirDrop Tray** to turn it on. Complete the Linux administrator password prompt.
2. On the iPhone, enable Wi-Fi/Bluetooth, select **Everyone for 10 Minutes**, then choose a file → Share → AirDrop.
3. Choose the computer's current hostname and accept the request on Linux.

Files go into the user's XDG download folder under **AirDrop** (`~/Downloads/AirDrop` as a fallback). The folder opens after the complete archive is saved and the success response is sent. Existing filenames receive numbered suffixes. Hidden `._` files are AppleDouble metadata supplied by Apple alongside the actual files.

Sessions last 15 minutes. An active outgoing transfer extends the deadline, then an extended session stops about a minute after it ends. Turning AirDrop off stops it immediately. The radio interface and temporary firewall exceptions are removed and the previous managed-interface state is restored on normal shutdown.

On `rtw89_8852ce`, automatic OWL/pin mode also starts a hidden WPA2 AP context on channel 44 using `wpa_supplicant`. It uses a fresh random key in a root-private `/run` directory and configures no DHCP, bridging or routing. File traffic continues over AWDL. This keeps the adapter's hardware MAC active; the observed speed gain suggests the monitor-only hardware state was a major bottleneck. Shutdown stops the context, removes its credentials and restores managed Wi-Fi mode. If setup fails, the receiver falls back to plain monitor mode. Other drivers do not automatically enable this workaround.

## Send to an iPhone

1. Unlock the phone, select **Everyone for 10 Minutes**, and keep Wi-Fi/Bluetooth enabled.
2. On this tested iOS setup, also open any photo → Share → AirDrop. This wakes the phone's receiving port when simply leaving it on the Home Screen does not.
3. Open **Send via AirDrop**, use the tray's Send files action, or select files in Dolphin → **Send via AirDrop…** if installed.
4. Choose the phone, click Send and accept on the iPhone. Keep the phone nearby until completion.

The sender reports success only after the phone acknowledges the upload. Connection establishment retries briefly; it never automatically repeats an acceptance request or an upload. Bluetooth advertises discovery information; file data travels over Wi-Fi AWDL, not Bluetooth.

From a source checkout:

```sh
python3 AirDropReceiver/receive.py
python3 AirDropReceiver/send-dialog.py --start-radio -- /path/to/file
python3 AirDropReceiver/stop.py
```

For diagnostics, with the radio already running:

```sh
AirDropReceiver/bin/luftlift -i awdl0 find --duration 5 --json
python3 AirDropReceiver/send-engine.py --address '[fe80::PEER_ADDRESS%ZONE]:8770' /path/to/file
```

Use the current address reported by discovery. Do not copy addresses from another machine or an older radio session.

## Configuration and troubleshooting

Set environment variables before launching the application:

| Variable | Purpose |
| --- | --- |
| `AIRDROP_WIFI_INTERFACE` | Select an unused Wi-Fi interface; otherwise a single available interface is chosen, preferring carl9170 if present |
| `AIRDROP_INCOMING_DIR` | Override the receive folder |
| `AIRDROP_STATE_DIR` | Override private session/log directory; defaults to `$XDG_STATE_HOME/airdrop-linux` or `~/.local/state/airdrop-linux` |
| `AIRDROP_BACKEND=auto/ owl/ filin` | Choose the radio implementation; auto uses OWL on rtw89 and filin elsewhere |
| `AIRDROP_CHANNEL_STRATEGY=auto/ pin/ verbatim/ rotate/ widen/ intersect` | OWL scheduling; auto uses pin on rtw89, verbatim elsewhere; strategies require hardware testing |
| `AIRDROP_MAC_CONTEXT=auto/ off/ ap` | Auto enables the temporary AP context only for `rtw89_8852ce` with OWL/pin and `wpa_supplicant`; `off` disables it; explicit `ap` requires OWL/pin and is unverified on other hardware |

Values contain no spaces: for example `AIRDROP_BACKEND=owl AIRDROP_CHANNEL_STRATEGY=pin python3 AirDropReceiver/receive.py`.

- **No device / connection refused:** refresh Everyone for 10 Minutes, reopen the iPhone Share → AirDrop screen and search again. A refusal can mean the phone has closed its receiving port.
- **Radio will not start:** check `radio.log`, polkit authentication, whether the adapter is connected, and monitor/injection capability.
- **Slow / stalls:** inspect the radio link and retransmissions. On RTL8852CE, check for `RADIO_CONTEXT=ap` in `radio.log` and install `wpa_supplicant` if absent. Set `AIRDROP_MAC_CONTEXT=off` to compare with plain monitor mode. BBR is enabled only on the sender's sockets when available, without changing the system TCP default. Bluetooth wake-up advertising stops once the peer accepts an upload; system Bluetooth stays enabled.
- **Stuck phone progress after a failed upload:** cancel the phone's transfer. If no Cancel is available, turn Wi-Fi off and on in Settings, select Everyone again and reopen AirDrop before retrying.
- **No tray on another desktop:** use the application-menu launchers or scripts. GNOME may need its own tray support; it is not tested here.

Logs can include nearby device names, addresses and filenames. They are kept locally and are excluded from Git and release bundles. Review/redact them before sharing a bug report.

Temporary firewall exceptions permit IPv6 link-local TCP 8771 and UDP 5353 only on `awdl0`. Saved firewall configuration is not modified. AirDrop interoperability uses self-signed peer TLS identities; it does not authenticate a peer using a public CA or an Apple account. The launcher's explicit consent remains necessary.

## Tests and sources

```sh
cargo test --manifest-path native/opendrop-rs/Cargo.toml --locked
python3 tests/check_sender.py
python3 tests/check_receiver.py
QT_QPA_PLATFORM=offscreen python3 tests/check_sender_dialog.py
python3 tests/check_install.py
python3 tests/check_radio_context.py
cmake --build native/owl/build --target tests
native/owl/build/tests/tests
```

An optional GitHub Actions build/test template is provided in `ci/github-actions.yml.example`; copy it to `.github/workflows/test.yml` to enable CI.

Tests cover real IPv6 TLS exchanges, client identities, binary plists, chunked responses, same-connection Ask/Upload, compressed/stored archive blocks, Unicode filenames, path containment, rejection, duplicate filenames, Qt process callbacks, and portable installation. They do not replace a real iPhone/hardware test.

Full modified build sources are in `native/`. See [THIRD_PARTY.md](THIRD_PARTY.md) for upstream commits, licenses and the local protocol/radio changes. GPL-3.0-only applies to this distribution, with third-party notices retained.

To uninstall (stop the receiver first):

```sh
python3 install.py --uninstall
# Include --prefix if you used a custom installation path.
```

Received files and local state/logs are retained.
