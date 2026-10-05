# AirDrop Linux

Send files between your iPhone and Linux PC using AirDrop.

- Receive files into **Downloads/AirDrop** and open the folder automatically.
- Send files from the desktop app or Dolphin’s right-click menu.
- Turn AirDrop on and off from the system tray.

## Install

Install the [dependencies](docs/setup.md#dependencies), then build from source:

```sh
git clone https://github.com/1raad1/airdrop-linux.git
cd airdrop-linux
./build.sh
python3 install.py
```

For Dolphin integration and the tray at login:

```sh
python3 install.py --kde --autostart
```

[CachyOS/Arch x86-64 downloads](https://github.com/1raad1/airdrop-linux/releases/latest) are also available. See the [setup guide](docs/setup.md) for installation options and other distributions.

## Use it

On the iPhone, enable Wi-Fi and Bluetooth and select **Everyone for 10 Minutes** in AirDrop settings.

**Receive on Linux:** Turn AirDrop on from the tray or open **AirDrop Receive**. Complete the Linux password prompt, then share a file from the iPhone → **AirDrop** → your PC’s hostname. Accept it on Linux.

**Send to iPhone:** Open **Send via AirDrop**, or right-click files in Dolphin → **Send via AirDrop…**. Choose the phone, click **Send** and accept on the iPhone. Keep the phone’s **Share → AirDrop** screen open so it stays discoverable.

## Compatibility

Experimental. Tested on **CachyOS / KDE Plasma**, **RTL8852CE Wi-Fi** and **iPhone 13 Pro (iOS 27.0)**. Other setups are unverified.

Your Wi-Fi adapter needs monitor mode and frame injection. AirDrop uses that adapter exclusively during a session; use Ethernet or another adapter for internet access.

On the tested PC, the same 2 MB TIFF upload improved from **233 seconds to 14 seconds**, confirmed twice. [Compatibility and performance details](docs/compatibility.md).

## More help

- [Setup and uninstall](docs/setup.md)
- [Configuration and troubleshooting](docs/troubleshooting.md)
- [Development and tests](docs/development.md)

Built on [OpenDrop Rust and OWL](THIRD_PARTY.md). [GPL-3.0 license](LICENSE).
