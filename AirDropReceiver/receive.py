#!/usr/bin/env python3
"""A temporary AirDrop session with administrator radio setup and user file I/O."""
import fcntl
import json
import os
from pathlib import Path
import signal
import shutil
import socket
import subprocess
import sys
import time

from common import BASE, STATE, INCOMING, prepare_state, is_receiver
NAME = socket.gethostname()
stop_requested = False


def notify(message):
    if shutil.which("notify-send"):
        subprocess.run(["notify-send", "AirDrop", message], check=False)
    else:
        print(message, file=sys.stderr, flush=True)


def error(message):
    subprocess.run([sys.executable, str(BASE / "desktop-helper.py"), "--title", "AirDrop", "--error", message], check=False)


def stop(*_):
    global stop_requested
    stop_requested = True


def radio_ready():
    result = subprocess.run(["ip", "-j", "-6", "address", "show", "dev", "awdl0"],
                            capture_output=True, text=True)
    if result.returncode:
        return False
    return any(a.get("scope") == "link" and not a.get("tentative", False)
               and not a.get("dadfailed", False) and "tentative" not in a.get("flags", [])
               for entry in json.loads(result.stdout) for a in entry.get("addr_info", []))


def interface_index():
    try:
        return (Path("/sys/class/net/awdl0/ifindex")).read_text().strip()
    except FileNotFoundError:
        return None


def sender_active():
    for marker in STATE.glob("sender-*/active.json"):
        try:
            record = json.loads(marker.read_text())
            process = Path("/proc") / str(int(record["pid"]))
            started = (process / "stat").read_text().rsplit(")", 1)[1].split()[19]
            arguments = (process / "cmdline").read_bytes().split(b"\0")
            if started == record["started"] and len(arguments) > 1 and Path(os.fsdecode(arguments[1])).resolve() == BASE / "send-engine.py":
                return True
        except (OSError, ValueError, KeyError, IndexError, TypeError):
            continue
    return False


def wifi_interface():
    requested = os.environ.get("AIRDROP_WIFI_INTERFACE")
    if requested:
        return requested
    candidates = sorted(p for p in Path("/sys/class/net").iterdir()
                        if (p / "phy80211").exists() and (p / "type").read_text().strip() == "1")
    for candidate in candidates:
        if (candidate / "device/driver").resolve().name == "carl9170":
            return candidate.name
    if len(candidates) == 1:
        return candidates[0].name
    if len(candidates) > 1:
        if not shutil.which("kdialog"):
            error("Several Wi-Fi radios found. Set AIRDROP_WIFI_INTERFACE to one of: " + ", ".join(p.name for p in candidates))
            return None
        choices = [item for p in candidates for item in (p.name, p.name)]
        result = subprocess.run(["kdialog", "--title", "AirDrop Wi-Fi radio", "--menu",
                                 "Choose an unused Wi-Fi adapter:", *choices], capture_output=True, text=True)
        return result.stdout.strip() if result.returncode == 0 else None
    return None


def main():
    prepare_state(STATE)
    lock = (STATE / "lock").open("w")
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        notify("Already running. Use Stop AirDrop Receiver to end the session.")
        return 1
    iface = wifi_interface()
    if not iface:
        error("No Wi-Fi radio selected or available.")
        return 1
    (STATE / "pid").write_text(str(os.getpid()))
    (STATE / "mode").unlink(missing_ok=True)
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    INCOMING.mkdir(parents=True, exist_ok=True)
    radio = receiver = None
    log = (STATE / "radio.log").open("w")
    receive_log = (STATE / "receiver.log").open("w")
    try:
        notify("Authenticate in the Linux password prompt to start the AirDrop radio.")
        radio = subprocess.Popen(["pkexec", "/usr/bin/bash", str(BASE / "radio.sh"), iface,
                                  os.environ.get("AIRDROP_BACKEND", "auto"), os.environ.get("AIRDROP_CHANNEL_STRATEGY", "auto"),
                                  os.environ.get("AIRDROP_MAC_CONTEXT", "auto")],
                                 stdin=subprocess.PIPE, stdout=log, stderr=subprocess.STDOUT,
                                 start_new_session=True)
        deadline = time.monotonic() + 120
        while not stop_requested and time.monotonic() < deadline:
            if radio.poll() is not None:
                error("The radio could not start. Administrator authentication may have been cancelled.\n\nDetails: " + str(STATE / "radio.log"))
                return 1
            if radio_ready():
                break
            time.sleep(0.25)
        if stop_requested:
            return 0
        if not radio_ready():
            error("The Wi-Fi radio did not become ready.\nDetails: " + str(STATE / "radio.log"))
            return 1
        env = os.environ.copy()
        env["LUFTLIFT_CONFIRM"] = str(BASE / "desktop-helper.py")
        env["LUFTLIFT_OPEN_RECEIVED_FOLDER"] = str(BASE / "desktop-helper.py")
        env["RUST_LOG"] = "luftlift_rs=debug,info,mdns_sd=off"
        receiver = subprocess.Popen([str(BASE / "bin" / "luftlift"), "-i", "awdl0", "receive",
                                     "--name", NAME, "-o", str(INCOMING)], env=env,
                                    stdout=receive_log, stderr=subprocess.STDOUT)
        active_index = interface_index()
        notify("Listening for 15 minutes. Discovery depends on Wi-Fi driver compatibility. On iPhone: Share → AirDrop → " + NAME + ".")
        print("AirDrop receiver ready: " + NAME, flush=True)
        deadline = time.monotonic() + 900
        last_keepalive = 0
        known_files = set(INCOMING.iterdir())
        while not stop_requested:
            now = time.monotonic()
            if sender_active():
                deadline = max(deadline, now + 60)
                if now - last_keepalive >= 30:
                    radio.stdin.write(b"KeepAlive\n")
                    radio.stdin.flush()
                    last_keepalive = now
            if now >= deadline:
                break
            command_file = STATE / "mode"
            if command_file.exists():
                command = command_file.read_text().strip()
                command_file.unlink(missing_ok=True)
                if command in ("owl", "plain", "mixed"):
                    # Withdraw the service while its radio is still available.
                    receiver.send_signal(signal.SIGINT)
                    receiver.wait(timeout=5)
                    radio.stdin.write((command + "\n").encode())
                    radio.stdin.flush()
                    time.sleep(1)
                    for _ in range(60):
                        if radio_ready() and interface_index() != active_index:
                            break
                        time.sleep(0.25)
                    active_index = interface_index()
                    receiver = subprocess.Popen([str(BASE / "bin" / "luftlift"), "-i", "awdl0", "receive",
                                                 "--name", NAME, "-o", str(INCOMING)], env=env,
                                                stdout=receive_log, stderr=subprocess.STDOUT)
                    notify("Compatibility radio enabled. Reopen AirDrop on the iPhone.")
            if receiver.poll() is not None or radio.poll() is not None:
                error("The AirDrop session stopped unexpectedly.\nLogs: " + str(STATE))
                return 1
            current = set(INCOMING.iterdir())
            for file in sorted(current - known_files):
                if not file.name.startswith("."):
                    notify("Received " + file.name + " in " + str(INCOMING))
            known_files = current
            time.sleep(0.5)
        return 0
    finally:
        if receiver and receiver.poll() is None:
            receiver.send_signal(signal.SIGINT)
            try:
                receiver.wait(timeout=5)
            except subprocess.TimeoutExpired:
                receiver.kill()
                receiver.wait()
        if radio:
            if radio.stdin:
                try:
                    radio.stdin.write(b"Stop\n")
                    radio.stdin.flush()
                except BrokenPipeError:
                    pass
                try:
                    radio.stdin.close()
                except BrokenPipeError:
                    pass
            try:
                radio.wait(timeout=10)
            except subprocess.TimeoutExpired:
                notify("Radio cleanup is still running; see radio.log.")
        (STATE / "pid").unlink(missing_ok=True)
        log.close()
        receive_log.close()
        notify("Receiver stopped.")


if __name__ == "__main__":
    sys.exit(main())
