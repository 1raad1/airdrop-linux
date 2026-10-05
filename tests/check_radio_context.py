"""Exercise temporary AP setup/failure/cleanup without touching a real radio."""
import json
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent.parent
radio = (ROOT / "AirDropReceiver/radio.sh").read_text()
functions = radio[radio.index("stop_context() {"):radio.index("firewall_rule() {")]
with tempfile.TemporaryDirectory(prefix="AirDrop radio test ") as temporary:
    root = Path(temporary)
    commands = root / "commands"
    commands.mkdir()
    for name in ("cat", "chmod", "openssl", "rg", "sleep", "rm"):
        commands.joinpath(name).symlink_to(Path("/usr/bin") / name)
    stub = '''#!/usr/bin/python3
import json, os, pathlib, signal, stat, subprocess, sys, time
name = pathlib.Path(sys.argv[0]).name
root = pathlib.Path(os.environ['RADIO_FIXTURE'])
if name == 'mktemp':
    result = subprocess.run(['/usr/bin/mktemp', '-d', str(root / 'context.XXXXXXXX')], capture_output=True, text=True, check=True)
    print(result.stdout.strip())
elif name == 'wpa_supplicant':
    config = pathlib.Path(sys.argv[sys.argv.index('-c') + 1])
    text = config.read_text()
    assert stat.S_IMODE(config.stat().st_mode) == 0o600
    assert stat.S_IMODE(config.parent.stat().st_mode) == 0o700
    assert 'ignore_broadcast_ssid=1' in text and 'frequency=5220' in text
    assert 'key_mgmt=WPA-PSK' in text and 'pairwise=CCMP' in text
    secret = next(line.strip()[4:] for line in text.splitlines() if line.strip().startswith('psk='))
    assert len(secret) == 64 and all(c in '0123456789abcdef' for c in secret)
    if os.environ.get('WPA_FIXTURE_FAIL') == '1': sys.exit(1)
    print('AP-ENABLED', flush=True)
    while True: time.sleep(1)
else:
    with (root / 'commands.jsonl').open('a') as output:
        output.write(json.dumps([name, *sys.argv[1:]]) + '\\n')
'''
    for name in ("mktemp", "wpa_supplicant", "ip", "iw"):
        command = commands / name
        command.write_text(stub)
        command.chmod(0o755)
    script = root / "fixture.sh"
    script.write_text("set -euo pipefail\niface=fixture0\ncontext_pid=''\ncontext_dir=''\n" + functions + '''
trap stop_context EXIT
if start_context; then
    echo STARTED
else
    echo FALLBACK
    stop_context
fi
stop_context
[[ -z $context_pid && -z $context_dir ]]
''')
    environment = dict(os.environ, PATH=str(commands), RADIO_FIXTURE=str(root))
    for failure in (False, True):
        environment["WPA_FIXTURE_FAIL"] = "1" if failure else "0"
        result = subprocess.run(["/usr/bin/bash", str(script)], env=environment,
                                capture_output=True, text=True, timeout=15, check=True)
        assert ("FALLBACK" if failure else "STARTED") in result.stdout, (result.stdout, result.stderr)
        assert not list(root.glob("context.*")), "Private credentials were not removed"
        calls = [json.loads(line) for line in (root / "commands.jsonl").read_text().splitlines()]
        assert ["ip", "link", "set", "fixture0", "down"] in calls
        assert ["iw", "dev", "fixture0", "set", "type", "managed"] in calls
    # A missing optional binary falls back without creating credentials.
    (commands / "wpa_supplicant").unlink()
    result = subprocess.run(["/usr/bin/bash", str(script)], env=environment,
                            capture_output=True, text=True, timeout=15, check=True)
    assert "FALLBACK" in result.stdout and not list(root.glob("context.*"))
print("Private WPA2 context, optional dependency, failed startup and idempotent managed-mode cleanup: PASS")
