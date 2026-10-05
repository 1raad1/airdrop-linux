#!/usr/bin/env python3
import fcntl
import os
from pathlib import Path
import signal

from common import STATE as state, is_receiver
try:
    with (state / "lock").open("a") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            pid = int((state / "pid").read_text())
            if is_receiver(pid):
                os.kill(pid, signal.SIGTERM)
except (FileNotFoundError, ProcessLookupError, ValueError):
    pass
