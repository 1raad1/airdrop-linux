"""User-specific paths and process checks shared by the desktop launchers."""
import os
from pathlib import Path
import subprocess

BASE = Path(__file__).resolve().parent
STATE = Path(os.environ.get("AIRDROP_STATE_DIR", str(Path(os.environ.get("XDG_STATE_HOME", Path.home() / ".local/state")) / "airdrop-linux"))).expanduser()

def incoming_directory():
    configured = os.environ.get("AIRDROP_INCOMING_DIR")
    if configured:
        return Path(configured).expanduser().absolute()
    downloads = Path.home() / "Downloads"
    try:
        result = subprocess.run(["xdg-user-dir", "DOWNLOAD"], capture_output=True, text=True, timeout=2)
        if result.returncode == 0 and Path(result.stdout.strip()).is_absolute():
            downloads = Path(result.stdout.strip())
    except (OSError, subprocess.TimeoutExpired):
        pass
    return downloads / "AirDrop"

INCOMING = incoming_directory()

def prepare_state(directory=STATE):
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    directory.chmod(0o700)

def is_receiver(pid):
    try:
        arguments = Path(f"/proc/{pid}/cmdline").read_bytes().split(b"\0")
        return len(arguments) > 1 and Path(os.fsdecode(arguments[1])).resolve() == BASE / "receive.py"
    except (OSError, ValueError):
        return False
