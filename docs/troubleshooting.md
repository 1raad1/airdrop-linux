# Configuration and troubleshooting

[Back to README](../README.md)

## Configuration

Set environment variables before launching the application:

| Variable | Purpose |
| --- | --- |
| `AIRDROP_WIFI_INTERFACE` | Select an unused Wi-Fi interface; otherwise a single available interface is chosen, preferring carl9170 if present |
| `AIRDROP_INCOMING_DIR` | Override the receive folder |
| `AIRDROP_STATE_DIR` | Override private session/log directory; defaults to `$XDG_STATE_HOME/airdrop-linux` or `~/.local/state/airdrop-linux` |
| `AIRDROP_BACKEND=auto/ owl/ filin` | Choose the radio implementation; auto uses OWL on rtw89 and filin elsewhere |
| `AIRDROP_CHANNEL_STRATEGY=auto/ pin/ verbatim/ rotate/ widen/ intersect` | OWL scheduling; auto uses pin on rtw89, verbatim elsewhere; strategies require hardware testing |
| `AIRDROP_MAC_CONTEXT=auto/ off/ ap` | Auto enables the temporary AP context only for `rtw89_8852ce` with OWL/pin and `wpa_supplicant`; `off` disables it; explicit `ap` requires OWL/pin and is unverified on other hardware |

Backend and strategy choices contain no spaces: for example `AIRDROP_BACKEND=owl AIRDROP_CHANNEL_STRATEGY=pin python3 AirDropReceiver/receive.py`.

## Common problems

- **No device / connection refused:** refresh Everyone for 10 Minutes, reopen the iPhone Share → AirDrop screen and search again. A refusal can mean the phone has closed its receiving port.
- **Radio will not start:** check `radio.log`, polkit authentication, whether the adapter is connected, and monitor/injection capability.
- **Slow / stalls:** inspect the radio link and retransmissions. On RTL8852CE, check for `RADIO_CONTEXT=ap` in `radio.log` and install `wpa_supplicant` if absent. Set `AIRDROP_MAC_CONTEXT=off` to compare with plain monitor mode. BBR is enabled only on the sender's sockets when available, without changing the system TCP default. Bluetooth wake-up advertising stops once the peer accepts an upload; system Bluetooth stays enabled.
- **Stuck phone progress after a failed upload:** cancel the phone's transfer. If no Cancel is available, turn Wi-Fi off and on in Settings, select Everyone again and reopen AirDrop before retrying.
- **No tray on another desktop:** use the application-menu launchers or scripts. GNOME may need its own tray support; it is not tested here.

Logs can include nearby device names, addresses and filenames. They are kept locally and are excluded from Git and release bundles. Review/redact them before sharing a bug report.

## Received files and session duration

Files go into the user's XDG download folder under **AirDrop** (`~/Downloads/AirDrop` as a fallback). The folder opens after the complete archive is saved and the success response is sent. Existing filenames receive numbered suffixes. Hidden `._` files are AppleDouble metadata supplied by Apple alongside the actual files.

Sessions last 15 minutes. An active outgoing transfer extends the deadline, then an extended session stops about a minute after it ends. Turning AirDrop off stops it immediately. The radio interface and temporary firewall exceptions are removed and the previous managed-interface state is restored on normal shutdown.

## Command line

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
