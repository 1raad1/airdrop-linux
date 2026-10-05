#!/usr/bin/env python3
"""Bundle tracked sources plus the three built binaries; exclude session data."""
import argparse
import hashlib
import io
import json
from pathlib import Path
import re
import subprocess
import tarfile

ROOT = Path(__file__).resolve().parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--version', required=True)
parser.add_argument('--platform', required=True)
args = parser.parse_args()
for value in (args.version, args.platform):
    if not re.fullmatch(r'[A-Za-z0-9._-]+', value): parser.error('Use a simple version/platform name.')
prefix = 'airdrop-linux-' + args.version
destination = ROOT / 'dist' / (prefix + '-' + args.platform + '.tar.gz')
destination.parent.mkdir(exist_ok=True)
files = subprocess.check_output(['git', 'ls-files', '-z'], cwd=ROOT).decode().split('\0')
files = [name for name in files if name]
files += ['AirDropReceiver/bin/' + name for name in ('luftlift', 'owl', 'filin')]
for name in files:
    if not (ROOT / name).is_file(): parser.error('Missing release file: ' + name)
info = json.dumps({'version': args.version, 'platform': args.platform,
                   'source_commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
                   'note': 'Platform-specific build; see README requirements and compatibility matrix.'}, indent=2).encode() + b'\n'
with tarfile.open(destination, 'w:gz') as archive:
    for name in files:
        archive.add(ROOT / name, arcname=prefix + '/' + name, recursive=False)
    metadata = tarfile.TarInfo(prefix + '/BUILD-INFO.json')
    metadata.size = len(info); metadata.mode = 0o644
    archive.addfile(metadata, io.BytesIO(info))
checksum = hashlib.sha256(destination.read_bytes()).hexdigest()
(destination.parent / 'SHA256SUMS').write_text(checksum + '  ' + destination.name + '\n')
print(destination)
