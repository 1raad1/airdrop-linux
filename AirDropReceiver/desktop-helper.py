#!/usr/bin/env python3
"""Consent and folder opening across KDE, GTK and Qt desktops."""
import argparse
import os
from pathlib import Path
import shutil
import subprocess
import sys

def dialog(title, message, error=False):
    if shutil.which("kdialog"):
        return subprocess.call(["kdialog", "--title", title, "--error" if error else "--yesno", message])
    if shutil.which("zenity"):
        return subprocess.call(["zenity", "--error" if error else "--question", "--title", title, "--text", message])
    if os.environ.get("DISPLAY") or os.environ.get("WAYLAND_DISPLAY"):
        try:
            from PySide6.QtWidgets import QApplication, QMessageBox
            app = QApplication.instance() or QApplication([])
            if error:
                QMessageBox.critical(None, title, message)
                return 0
            buttons = QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No
            reply = QMessageBox.question(None, title, message, buttons, QMessageBox.StandardButton.No)
            return 0 if reply == QMessageBox.StandardButton.Yes else 1
        except ImportError:
            pass
    print(title + ": " + message, file=sys.stderr)
    if error:
        return 0
    try:
        with open("/dev/tty", "r+") as terminal:
            terminal.write("Accept? [y/N] "); terminal.flush()
            return 0 if terminal.readline().strip().lower() == "y" else 1
    except OSError:
        return 1  # Receiving without a working consent UI is declined.

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--title", default="AirDrop")
    parser.add_argument("--yesno")
    parser.add_argument("--error")
    parser.add_argument("--new-window", type=Path)
    args = parser.parse_args()
    if args.new_window is not None:
        folder = str(args.new_window.absolute())
        if "KDE" in os.environ.get("XDG_CURRENT_DESKTOP", "") and shutil.which("dolphin"):
            return subprocess.call(["dolphin", "--new-window", folder])
        return subprocess.call(["xdg-open", folder])
    return dialog(args.title, args.error if args.error is not None else args.yesno or "", args.error is not None)

if __name__ == "__main__":
    sys.exit(main())
