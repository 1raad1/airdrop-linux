# Setup

[Back to README](../README.md)

## Dependencies

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

## Install a prebuilt release

Download a compatible archive from [Releases](https://github.com/1raad1/airdrop-linux/releases), extract it and open the extracted directory. Install the runtime dependencies above, then run:

```sh
python3 install.py
# Optional Dolphin menu and tray at login
python3 install.py --kde --autostart
```

Prebuilt archives already contain the native binaries, so you can skip `build.sh`.

## Uninstall

Stop the receiver first.

```sh
python3 install.py --uninstall
# Include --prefix if you used a custom installation path.
```

Received files and local state/logs are retained.
