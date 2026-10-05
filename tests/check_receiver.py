import http.client
import json
import os
from pathlib import Path
import plistlib
import signal
import socket
import ssl
import subprocess
import tempfile
import time
import zlib

binary = Path(__file__).resolve().parent.parent / "AirDropReceiver/bin/luftlift"
context = ssl._create_unverified_context()

def request(path, body=b"", content_type="application/x-apple-plist"):
    connection = http.client.HTTPSConnection("::1", 8771, context=context, timeout=5)
    connection.request("POST", path, body, {"Content-Type": content_type})
    response = connection.getresponse()
    result = response.status, response.read()
    connection.close()
    return result

def odc(name, payload, ino):
    name = name.encode() + b"\0"
    header = ("070707" + "000000" + f"{ino:06o}" + "100644" + "000000000000000001000000" +
              "00000000000" + f"{len(name):06o}{len(payload):011o}").encode()
    assert len(header) == 76
    return header + name + payload

with tempfile.TemporaryDirectory(prefix="AirDrop test ") as temporary:
    destination = Path(temporary)
    calls = destination / "opened.jsonl"
    opener = destination / "fake dolphin"
    opener.write_text("#!/usr/bin/python\nimport json, sys\nfrom pathlib import Path\nwith Path(" + repr(str(calls)) + ").open('a') as out: out.write(json.dumps(sys.argv[1:]) + '\n')\n".replace("'\n'", "'\\n'"))
    opener.chmod(0o755)
    for confirmation in ("/usr/bin/true", "/usr/bin/false"):
        environment = os.environ.copy()
        environment["LUFTLIFT_CONFIRM"] = confirmation
        environment["LUFTLIFT_OPEN_RECEIVED_FOLDER"] = str(opener)
        process = subprocess.Popen([str(binary), "-i", "lo", "receive", "--http-addr", "off",
                                    "--name", "Fixture Linux PC", "-o", str(destination)],
                                   env=environment, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            for _ in range(50):
                try:
                    status, body = request("/Discover")
                    break
                except OSError:
                    time.sleep(0.1)
            else:
                raise AssertionError("HTTPS listener did not start")
            assert status == 200
            assert plistlib.loads(body)["ReceiverComputerName"] == "Fixture Linux PC"
            # The receiver must bind only the IPv6 address of its chosen interface.
            with socket.socket() as ipv4:
                assert ipv4.connect_ex(("127.0.0.1", 8771)) != 0
            ask = plistlib.dumps({"SenderComputerName": "Test iPhone", "Files": [{"FileName": "photo.txt"}]})
            status, _ = request("/Ask", ask)
            if confirmation.endswith("false"):
                assert status == 403
                print("Rejected transfer: PASS")
                continue
            assert status == 200
            assert not calls.exists()
            payload = b"AirDrop smoke test\n"
            archive = odc("../../photo.txt", payload, 1) + odc("second.txt", b"another file", 2) + odc("TRAILER!!!", b"", 3)
            compressed = zlib.compress(archive)
            dvzip = len(compressed).to_bytes(4, "big") + compressed
            status, _ = request("/Upload", dvzip, "application/x-dvzip")
            assert status == 200
            assert (destination / "photo.txt").read_bytes() == payload
            assert request("/Upload", dvzip, "application/x-dvzip")[0] == 403
            assert request("/Ask", ask)[0] == 200
            assert request("/Upload", dvzip, "application/x-dvzip")[0] == 200
            assert (destination / "photo (1).txt").read_bytes() == payload
            for _ in range(50):
                if calls.exists() and len(calls.read_text().splitlines()) == 2:
                    break
                time.sleep(0.1)
            opened = [json.loads(line) for line in calls.read_text().splitlines()]
            assert opened == [["--new-window", str(destination)]] * 2, opened
            print("Folder opened once per completed multi-file transfer; paths with spaces: PASS")
            print("Discover, approval, dvzip file receive, path containment, duplicate preservation, unsolicited-upload rejection, interface-only binding: PASS")
        finally:
            process.send_signal(signal.SIGINT)
            process.wait(timeout=5)
