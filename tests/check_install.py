"""Install/uninstall in isolated XDG directories, including unusual paths."""
import configparser
import os
from pathlib import Path
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parent.parent
with tempfile.TemporaryDirectory(prefix='AirDrop installer test ') as temporary:
    root = Path(temporary)
    prefix = root / 'app with spaces $money `tick` %percent'
    data, config = root / 'xdg data', root / 'xdg config'
    environment = dict(os.environ, XDG_DATA_HOME=str(data), XDG_CONFIG_HOME=str(config))
    command = [sys.executable, str(ROOT / 'install.py'), '--prefix', str(prefix)]
    subprocess.run([*command, '--kde', '--autostart'], env=environment, check=True, capture_output=True)
    app = prefix / 'AirDropReceiver'
    assert (app / 'send-engine.py').is_file()
    assert not (app / '__pycache__').exists()
    service = data / 'kio/servicemenus/airdrop-linux-send.desktop'
    desktop = configparser.ConfigParser(interpolation=None)
    desktop.read(service)
    assert desktop['Desktop Entry']['Type'] == 'Service'
    assert desktop['Desktop Entry']['X-KDE-Protocols'] == 'file'
    execution = desktop['Desktop Action sendViaAirDrop']['Exec']
    assert '\\$money' in execution and '\\`tick\\`' in execution and '%%percent' in execution
    assert execution.endswith(' --start-radio -- %F')
    assert (config / 'autostart/airdrop-linux-tray.desktop').is_file()
    assert len(list((data / 'applications').glob('*.desktop'))) == 4
    # A normal update of our own installation is permitted.
    subprocess.run(command, env=environment, check=True, capture_output=True)
    received = root / 'downloads/AirDrop/keep.txt'
    received.parent.mkdir(parents=True); received.write_text('keep this')
    subprocess.run([*command, '--uninstall'], env=environment, check=True, capture_output=True)
    assert not prefix.exists() and not service.exists()
    assert not (config / 'autostart/airdrop-linux-tray.desktop').exists()
    assert not list((data / 'applications').glob('*.desktop'))
    assert received.read_text() == 'keep this'
    prefix.mkdir(); (prefix / 'unrelated.txt').write_text('unrelated')
    result = subprocess.run(command, env=environment, capture_output=True, text=True)
    assert result.returncode != 0 and 'not an AirDrop Linux installation' in result.stderr
    assert (prefix / 'unrelated.txt').read_text() == 'unrelated'
print('Portable paths, optional KDE/autostart entries, safe update/uninstall and existing-directory protection: PASS')
