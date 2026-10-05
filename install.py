#!/usr/bin/env python3
"""Install only this application and its optional desktop integration."""
import argparse
import os
from pathlib import Path
import shutil
import sys

ROOT = Path(__file__).resolve().parent

def quoted(value):
    # Desktop Exec parsing has its own escaping; it is not a shell command.
    return '"' + str(value).replace('\\', '\\\\').replace('"', '\\"').replace('`', '\\`').replace('$', '\\$').replace('%', '%%') + '"'

def entry(name, script, app):
    return ('[Desktop Entry]\nType=Application\nName=' + name + '\nExec=' + quoted(sys.executable) + ' ' + quoted(app / script) +
            '\nIcon=' + str(app / 'airdrop-on.svg') + '\nTerminal=false\nCategories=Network;FileTransfer;\n')

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--prefix', type=Path, default=Path(os.environ.get('XDG_DATA_HOME', Path.home() / '.local/share')) / 'airdrop-linux')
    parser.add_argument('--kde', action='store_true', help='Install Dolphin Send via AirDrop menu')
    parser.add_argument('--autostart', action='store_true', help='Start the tray at login; the radio stays off')
    parser.add_argument('--no-desktop', action='store_true', help='Install core scripts without desktop menu entries')
    parser.add_argument('--uninstall', action='store_true')
    args = parser.parse_args()
    prefix = args.prefix.expanduser().absolute()
    data = Path(os.environ.get('XDG_DATA_HOME', Path.home() / '.local/share'))
    config = Path(os.environ.get('XDG_CONFIG_HOME', Path.home() / '.config'))
    entries = [data / 'applications' / ('airdrop-linux-' + name + '.desktop') for name in ('receive', 'stop', 'tray', 'send')]
    service = data / 'kio/servicemenus/airdrop-linux-send.desktop'
    autostart = config / 'autostart/airdrop-linux-tray.desktop'
    if args.uninstall:
        for file in [*entries, service, autostart]: file.unlink(missing_ok=True)
        # Only delete a prefix with our own installer marker.
        if (prefix / '.airdrop-linux-install').is_file(): shutil.rmtree(prefix)
        print('Removed application and launcher entries. Received files and state were retained.')
        return 0
    if os.geteuid() == 0:
        parser.error('Run the installer as your regular desktop user, without sudo.')
    missing = [name for name in ('luftlift', 'owl', 'filin') if not (ROOT / 'AirDropReceiver/bin' / name).is_file()]
    if missing:
        parser.error('Build the binaries first with ./build.sh (missing: ' + ', '.join(missing) + ').')
    if prefix == ROOT or ROOT in prefix.parents or prefix in ROOT.parents:
        parser.error('The installation prefix must be separate from the source checkout.')
    if prefix.exists() and not (prefix / '.airdrop-linux-install').is_file():
        parser.error('The destination already exists and is not an AirDrop Linux installation.')
    app = prefix / 'AirDropReceiver'
    shutil.copytree(ROOT / 'AirDropReceiver', app, dirs_exist_ok=True, ignore=shutil.ignore_patterns('__pycache__', '*.pyc'))
    prefix.joinpath('.airdrop-linux-install').write_text('AirDrop Linux\n')
    for helper in ('radio.sh', 'desktop-helper.py'):
        (app / helper).chmod(0o755)
    if not args.no_desktop:
        for path, name, script in zip(entries, ('AirDrop Receive', 'Stop AirDrop', 'AirDrop Tray', 'Send via AirDrop'), ('receive.py', 'stop.py', 'tray.py', 'send-dialog.py')):
            path.parent.mkdir(parents=True, exist_ok=True)
            text = entry(name, script, app)
            if script == 'send-dialog.py': text = text.replace('\nIcon=', ' --start-radio\nIcon=')
            path.write_text(text)
    if args.kde:
        service.parent.mkdir(parents=True, exist_ok=True)
        service.write_text('[Desktop Entry]\nType=Service\nName=Send via AirDrop\nMimeType=application/octet-stream;\nActions=sendViaAirDrop;\nX-KDE-Priority=TopLevel\nX-KDE-Protocols=file\nX-KDE-MinNumberOfUrls=1\n\n[Desktop Action sendViaAirDrop]\nName=Send via AirDrop…\nIcon=' + str(app / 'airdrop-on.svg') + '\nExec=' + quoted(sys.executable) + ' ' + quoted(app / 'send-dialog.py') + ' --start-radio -- %F\n')
        service.chmod(0o755)
    if args.autostart:
        autostart.parent.mkdir(parents=True, exist_ok=True)
        autostart.write_text(entry('AirDrop Tray', 'tray.py', app))
    print('Installed to ' + str(prefix))
    print('Start: ' + sys.executable + ' ' + str(app / 'receive.py'))
    return 0

if __name__ == '__main__':
    sys.exit(main())
