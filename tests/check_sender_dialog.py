"""Regression: use real QProcess signals, avoiding QDialog.finished collisions."""
import importlib.util
import sys
import json
import os
from pathlib import Path
import sys
import tempfile
from unittest.mock import patch

os.environ["QT_QPA_PLATFORM"] = "offscreen"
from PySide6.QtCore import QProcess, QTimer
from PySide6.QtWidgets import QApplication

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "AirDropReceiver"))
base = Path(__file__).resolve().parent.parent / "AirDropReceiver"
spec = importlib.util.spec_from_file_location("sender_dialog", base / "send-dialog.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
app = QApplication([])
app.setQuitOnLastWindowClosed(False)

with tempfile.TemporaryDirectory() as temporary:
    module.STATE = Path(temporary)
    # Disable only startup Bluetooth; discovery/transfer callbacks use real children.
    with patch.object(QProcess, "start"):
        dialog = module.SendDialog([base.parent / "AirDrop-test.txt"])
    dialog.timer.stop()
    def run(mode, code):
        dialog.mode = mode
        dialog.errors = ""
        dialog.job.start(sys.executable, ["-c", code])
        QTimer.singleShot(500, app.quit)
        app.exec()
        assert dialog.mode is None, "The completion callback did not run"
    peer = {"name": "iPhone fixture", "address": "[fe80::1234%33]:8770", "id": "fixture"}
    run("scan", "print(" + repr(json.dumps([peer])) + ")")
    assert dialog.recipients.currentText() == "iPhone fixture"
    assert dialog.send_button.isEnabled() and not dialog.retry.isActive()
    run("send", "pass")
    assert "successfully" in dialog.status.text()
    run("send", "import sys; print('Error: The iPhone declined the transfer.', file=sys.stderr); sys.exit(1)")
    assert "declined" in dialog.status.text() and "successfully" not in dialog.status.text()
    dialog.peers = []
    dialog.recipients.clear()
    run("scan", "print('[]')")
    assert dialog.retry.isActive() and not dialog.send_button.isEnabled()
    dialog.reject()
    assert not dialog.retry.isActive()
print("Real QProcess callbacks: recipient discovery, success, rejection, automatic retries, and cancellation: PASS")
