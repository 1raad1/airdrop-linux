#!/usr/bin/env python3
"""File and recipient selection for the native AirDrop sender."""
import argparse
import json
from pathlib import Path
import re
import socket
import subprocess
import sys
import time

from PySide6.QtCore import QProcess, QProcessEnvironment, QTimer
from PySide6.QtWidgets import (QApplication, QComboBox, QDialog, QFileDialog,
                              QHBoxLayout, QLabel, QListWidget, QPushButton,
                              QMessageBox, QProgressBar, QVBoxLayout)

from common import BASE, STATE, prepare_state


class SendDialog(QDialog):
    def __init__(self, files=()):
        super().__init__()
        self.setWindowTitle("Send with AirDrop")
        self.resize(480, 380)
        self.files = [str(file) for file in files]
        self.peers = []
        self.mode = None
        self.bluetooth_error = ""
        self.radio_deadline = time.monotonic() + 125
        layout = QVBoxLayout(self)
        instructions = QLabel("Unlock your iPhone and set AirDrop to Everyone for 10 Minutes.\nOpen a photo → Share → AirDrop and leave that screen open.\nKeep Wi-Fi and Bluetooth on and the phone nearby.")
        instructions.setWordWrap(True)
        layout.addWidget(instructions)
        self.choose = QPushButton("Choose files…")
        self.choose.clicked.connect(self.choose_files)
        layout.addWidget(self.choose)
        self.file_list = QListWidget()
        self.file_list.addItems([Path(file).name for file in self.files])
        layout.addWidget(self.file_list)
        layout.addWidget(QLabel("Nearby AirDrop devices:"))
        row = QHBoxLayout()
        self.recipients = QComboBox()
        row.addWidget(self.recipients, 1)
        self.search = QPushButton("Search for devices")
        self.search.clicked.connect(self.scan)
        row.addWidget(self.search)
        layout.addLayout(row)
        self.status = QLabel("Starting AirDrop… Complete the Linux password prompt if shown.")
        self.status.setWordWrap(True)
        layout.addWidget(self.status)
        self.progress = QProgressBar()
        self.progress.setRange(0, 0)
        self.progress.hide()
        layout.addWidget(self.progress)
        buttons = QHBoxLayout()
        buttons.addStretch()
        self.close_button = QPushButton("Close")
        self.close_button.clicked.connect(self.reject)
        buttons.addWidget(self.close_button)
        self.send_button = QPushButton("Send")
        self.send_button.setEnabled(False)
        self.send_button.clicked.connect(self.send)
        buttons.addWidget(self.send_button)
        layout.addLayout(buttons)
        self.job = QProcess(self)
        environment = QProcessEnvironment.systemEnvironment()
        environment.insert("LUFTLIFT_NAME", socket.gethostname())
        environment.insert("RUST_LOG", "info,mdns_sd=off")
        self.job.setProcessEnvironment(environment)
        self.job.finished.connect(self.job_finished)
        self.job.readyReadStandardError.connect(self.read_progress)
        self.job.errorOccurred.connect(self.process_error)
        self.retry = QTimer(self)
        self.retry.setSingleShot(True)
        self.retry.timeout.connect(self.scan)
        self.ble = QProcess(self)
        self.ble.readyReadStandardError.connect(self.read_bluetooth_status)
        self.ble.readyReadStandardOutput.connect(self.read_bluetooth_status)
        self.ble.start(sys.executable, [str(BASE / "ble-wake.py")])
        self.timer = QTimer(self)
        self.timer.timeout.connect(self.wait_for_radio)
        self.timer.start(500)
        self.show()

    def wait_for_radio(self):
        if Path("/sys/class/net/awdl0").exists():
            self.timer.stop()
            self.scan()
        elif time.monotonic() >= self.radio_deadline:
            self.timer.stop()
            self.progress.hide()
            self.status.setText("AirDrop could not start. Enable it from the tray, then search again.")

    def choose_files(self):
        files, _ = QFileDialog.getOpenFileNames(self, "Choose files to AirDrop", str(Path.home()))
        if files:
            self.files = files
            self.file_list.clear()
            self.file_list.addItems([Path(file).name for file in files])
            self.update_send()

    def update_send(self):
        self.send_button.setEnabled(bool(self.files and self.peers) and self.mode is None)

    def scan(self):
        if self.mode is not None:
            return
        self.retry.stop()
        if not Path("/sys/class/net/awdl0").exists():
            self.status.setText("Enable AirDrop from the tray, then search again.")
            return
        self.peers = []
        self.recipients.clear()
        if self.ble.state() == QProcess.ProcessState.NotRunning:
            self.ble.start(sys.executable, [str(BASE / "ble-wake.py")])
        self.start_job("scan", ["-i", "awdl0", "find", "--duration", "5", "--json"])
        self.status.setText("Searching for nearby AirDrop devices…")

    def start_job(self, mode, arguments):
        self.retry.stop()
        self.mode = mode
        self.errors = ""
        self.progress.setRange(0, 0)
        self.progress.show()
        self.search.setEnabled(False)
        self.recipients.setEnabled(False)
        self.choose.setEnabled(mode != "send")
        self.send_button.setEnabled(False)
        self.close_button.setText("Cancel transfer" if mode == "send" else "Close")
        if mode == "send":
            self.job.start(sys.executable, [str(BASE / "send-engine.py"), *arguments])
        else:
            self.job.start(str(BASE / "bin" / "luftlift"), arguments)

    def send(self):
        peer = self.peers[self.recipients.currentIndex()]
        if self.ble.state() == QProcess.ProcessState.NotRunning:
            self.ble.start(sys.executable, [str(BASE / "ble-wake.py")])
        self.start_job("send", ["--skip-discover", "--address", peer["address"], *self.files])
        self.status.setText("Connecting to " + peer["name"] + "…")

    def read_progress(self):
        text = bytes(self.job.readAllStandardError()).decode(errors="replace")
        self.errors += text
        self.log(text)
        if self.mode == "send":
            if "Preparing files" in text:
                self.status.setText("Preparing files for AirDrop…")
            if "Waiting for iPhone AirDrop connection" in text:
                self.status.setText("Waiting for the iPhone to receive AirDrop… Open a photo → Share → AirDrop on the phone and leave it open.")
            if "POST /Ask" in text:
                self.status.setText("Waiting for you to accept the AirDrop on your iPhone…")
            if "POST /Upload" in text:
                self.status.setText("Sending files… Keep the phone nearby.")
            percentages = re.findall(r"Upload progress: (\d+)", text)
            if percentages:
                percent = int(percentages[-1])
                self.progress.setRange(0, 100)
                self.progress.setValue(percent)
                self.status.setText(f"Sending files… {percent}% sent. Keep the phone nearby.")

    def log(self, text):
        if text:
            prepare_state(STATE)
            with (STATE / "sender.log").open("a") as output:
                output.write(time.strftime("%Y-%m-%d %H:%M:%S ") + text)

    def read_bluetooth_status(self):
        error = bytes(self.ble.readAllStandardError()).decode(errors="replace").strip()
        output = bytes(self.ble.readAllStandardOutput()).decode(errors="replace")
        if error:
            self.bluetooth_error = error
        if "wake-up active" in output:
            self.bluetooth_error = ""
        self.log(error + output)

    def process_error(self, error):
        if error == QProcess.ProcessError.FailedToStart:
            self.job_finished(1, QProcess.ExitStatus.NormalExit)

    def job_finished(self, code, _):
        mode = self.mode
        self.mode = None
        self.read_progress()
        output = bytes(self.job.readAllStandardOutput()).decode(errors="replace")
        self.log(f"{mode} finished with code {code}; output: {output}\n")
        self.progress.hide()
        self.search.setEnabled(True)
        self.recipients.setEnabled(True)
        self.choose.setEnabled(True)
        self.close_button.setText("Close")
        if code != 0:
            reason = next((line.removeprefix("Error: ") for line in reversed(self.errors.splitlines()) if line.startswith("Error:")), "The connection failed.")
            self.status.setText("Could not " + ("send the files" if mode == "send" else "find devices") + ": " + reason + "\nOpen Share → AirDrop on the iPhone and try again.")
            if mode == "send" and ("Connection refused" in reason or "not accepting AirDrop connections" in reason):
                self.peers = []
                self.recipients.clear()
                self.retry.start(3000)
        elif mode == "scan":
            try:
                self.peers = json.loads(output)
            except (ValueError, TypeError):
                self.peers = []
            self.recipients.addItems([peer["name"] for peer in self.peers])
            self.log("Displayed devices: " + ", ".join(self.recipients.itemText(i) for i in range(self.recipients.count())) + "\n")
            self.status.setText("Choose files and a device, then click Send." if self.peers else
                                "Still looking for your iPhone… Enable Everyone for 10 Minutes, then open a photo → Share → AirDrop.")
        else:
            self.status.setText(f"Sent {len(self.files)} file(s) successfully.")
        self.update_send()
        if mode == "scan" and not self.peers:
            if self.bluetooth_error:
                self.status.setText("Bluetooth discovery needs attention: " + self.bluetooth_error)
            self.retry.start(3000)

    def reject(self):
        self.timer.stop()
        self.retry.stop()
        for process in (self.job, self.ble):
            if process.state() != QProcess.ProcessState.NotRunning:
                process.terminate()
                if not process.waitForFinished(1000):
                    process.kill()
                    process.waitForFinished(1000)
        super().reject()


def main():
    parser = argparse.ArgumentParser(description="Send selected files with AirDrop.")
    parser.add_argument("--start-radio", action="store_true")
    parser.add_argument("files", nargs="*", type=Path)
    args = parser.parse_args()
    app = QApplication(sys.argv)
    app.setApplicationName("AirDrop")
    files = [path.absolute() for path in args.files]
    for path in files:
        if not path.is_file():
            QMessageBox.critical(None, "Send with AirDrop", f"Select local files to send.\n\n{path} is not a file.")
            return 1
    if args.start_radio:
        # Reuse the same supervisor and password prompt as the tray toggle.
        from tray import STATE, receiver_pid
        if receiver_pid() is None:
            prepare_state(STATE)
            with (STATE / "tray-receiver.log").open("a") as log:
                subprocess.Popen([sys.executable, str(BASE / "receive.py")],
                                 stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
                                 start_new_session=True)
    dialog = SendDialog(files)
    return app.exec()


if __name__ == "__main__":
    sys.exit(main())
