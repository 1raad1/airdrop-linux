#!/usr/bin/env python3
"""AirDrop controls for the Plasma system tray; the radio stays off at login."""
import fcntl
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys

from PySide6.QtCore import QTimer
from PySide6.QtGui import QIcon
from PySide6.QtWidgets import QApplication, QMenu, QSystemTrayIcon

from common import BASE, STATE, INCOMING, prepare_state, is_receiver


def receiver_pid():
    """Ignore stale PID files and unrelated processes that reused a PID."""
    try:
        pid = int((STATE / "pid").read_text())
        arguments = Path(f"/proc/{pid}/cmdline").read_bytes().split(b"\0")
        if is_receiver(pid):
            return pid
    except (OSError, ValueError):
        pass
    return None


def receiver_ready(pid):
    try:
        children = Path(f"/proc/{pid}/task/{pid}/children").read_text().split()
        for child in children:
            arguments = Path(f"/proc/{child}/cmdline").read_bytes().split(b"\0")
            if arguments and os.fsdecode(arguments[0]) == str(BASE / "bin" / "luftlift"):
                return True
    except OSError:
        pass
    return False


class AirDropTray:
    def __init__(self, app):
        self.app = app
        self.launcher = None
        self.stopping = False
        self.state = ""
        self.tray = QSystemTrayIcon(app)
        self.icons = {name: QIcon(str(BASE / f"airdrop-{name}.svg")) for name in ("on", "off")}
        self.menu = QMenu()
        self.status = self.menu.addAction("AirDrop")
        self.status.setEnabled(False)
        self.switch = self.menu.addAction("AirDrop enabled")
        self.switch.setCheckable(True)
        self.switch.triggered.connect(self.toggle)
        self.menu.addSeparator()
        self.menu.addAction("Send files…", self.send_files)
        self.menu.addAction("Open AirDrop folder", self.open_folder)
        self.menu.addSeparator()
        self.menu.addAction("Quit tray icon", app.quit)
        self.tray.setContextMenu(self.menu)
        self.tray.activated.connect(self.activate)
        self.timer = QTimer(app)
        self.timer.timeout.connect(self.refresh)
        self.timer.start(500)
        self.refresh()
        self.tray.show()

    def refresh(self):
        child_running = self.launcher is not None and self.launcher.poll() is None
        pid = receiver_pid()
        if pid or child_running:
            state = "stopping" if self.stopping else ("on" if pid and receiver_ready(pid) else "starting")
        else:
            state = "off"
            self.stopping = False
        if state != self.state:
            self.state = state
            label = {"off": "Off", "starting": "Starting…", "on": "On", "stopping": "Stopping…"}[state]
            self.status.setText("AirDrop: " + label)
            self.switch.setChecked(state != "off")
            self.switch.setEnabled(state != "stopping")
            self.tray.setIcon(self.icons["on" if state == "on" else "off"])
            self.tray.setToolTip(f"AirDrop — {label}\n{socket.gethostname()}\nClick to toggle; right-click for options")
            print("AirDrop tray: " + state, flush=True)

    def activate(self, reason):
        if reason == QSystemTrayIcon.ActivationReason.Trigger:
            self.toggle()

    def toggle(self, *_):
        self.refresh()
        if self.state == "stopping":
            return
        if self.state != "off":
            # Use the existing stop helper so the receiver withdraws discovery
            # and the privileged helper restores its Wi-Fi/firewall state.
            subprocess.run([sys.executable, str(BASE / "stop.py")], check=False)
            if receiver_pid() is None and self.launcher and self.launcher.poll() is None:
                self.launcher.terminate()
            self.stopping = True
        else:
            with (STATE / "tray-receiver.log").open("a") as log:
                self.launcher = subprocess.Popen(
                    [sys.executable, str(BASE / "receive.py")],
                    stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
                    start_new_session=True)
        self.refresh()

    def open_folder(self):
        INCOMING.mkdir(parents=True, exist_ok=True)
        subprocess.Popen([sys.executable, str(BASE / "desktop-helper.py"), "--new-window", str(INCOMING)],
                         stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                         stderr=subprocess.DEVNULL, start_new_session=True)

    def send_files(self):
        self.refresh()
        if self.state == "off":
            self.toggle()
        subprocess.Popen([sys.executable, str(BASE / "send-dialog.py")],
                         stdin=subprocess.DEVNULL, start_new_session=True)


def main():
    prepare_state(STATE)
    lock = (STATE / "tray.lock").open("a")
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        return 0
    app = QApplication(sys.argv)
    app.setApplicationName("AirDrop")
    app.setApplicationDisplayName("AirDrop")
    app.setDesktopFileName("airdrop-linux-tray")
    app.setQuitOnLastWindowClosed(False)
    app.setWindowIcon(QIcon(str(BASE / "airdrop-on.svg")))
    tray = AirDropTray(app)
    signal.signal(signal.SIGTERM, lambda *_: app.quit())
    signal.signal(signal.SIGINT, lambda *_: app.quit())
    return app.exec()


if __name__ == "__main__":
    sys.exit(main())
