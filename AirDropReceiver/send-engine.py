#!/usr/bin/env python3
"""Native AirDrop upload using the protocol verified with the nearby iPhone."""
import argparse
import base64
import errno
import http.client
import ipaddress
import json
import os
from pathlib import Path
import plistlib
import signal
import socket
import ssl
import subprocess
import sys
import tempfile
import time
import uuid
import zlib

from common import BASE, STATE, prepare_state
BLOCK = 128 * 1024


def uti(path):
    return {".jpg": "public.jpeg", ".jpeg": "public.jpeg", ".png": "public.png",
            ".heic": "public.heic", ".gif": "com.compuserve.gif", ".mov": "com.apple.quicktime-movie",
            ".mp4": "public.mpeg-4", ".tif": "public.tiff", ".tiff": "public.tiff",
            ".pdf": "com.adobe.pdf", ".txt": "public.plain-text",
            ".md": "public.plain-text", ".zip": "public.zip-archive"}.get(path.suffix.lower(), "public.data")


def odc_header(name, size, ino, mode=0o100644):
    encoded = os.fsencode(name) + b"\0"
    header = ("070707000000" + f"{ino:06o}{mode:06o}" + "000000000000000001000000" +
              "00000000000" + f"{len(encoded):06o}{size:011o}").encode()
    if len(header) != 76:
        raise ValueError("This file exceeds the AirDrop archive size limit.")
    return header + encoded


def prepare_payload(files, destination):
    # Spool the archive to disk: videos do not need to fit in RAM.
    pending = bytearray()
    def flush(block):
        compressed = zlib.compress(block)
        if len(compressed) >= len(block):
            destination.write((len(block) | 0x80000000).to_bytes(4, "big"))
            destination.write(block)
        else:
            destination.write(len(compressed).to_bytes(4, "big"))
            destination.write(compressed)
    def append(data):
        pending.extend(data)
        while len(pending) >= BLOCK:
            flush(bytes(pending[:BLOCK]))
            del pending[:BLOCK]
    append(odc_header(".", 0, 1, 0o040700))
    for ino, path in enumerate(files, 2):
        with path.open("rb") as source:
            size = os.fstat(source.fileno()).st_size
            append(odc_header("./" + path.name, size, ino))
            remaining = size
            while remaining:
                data = source.read(min(BLOCK, remaining))
                if not data:
                    raise ValueError(f"{path.name} changed while preparing the transfer.")
                append(data)
                remaining -= len(data)
            if source.read(1):
                raise ValueError(f"{path.name} changed while preparing the transfer.")
    append(odc_header("TRAILER!!!", 0, len(files) + 2))
    if pending:
        flush(bytes(pending))
    size = destination.tell()
    destination.seek(0)
    return size


class ScopedHTTPS(http.client.HTTPSConnection):
    def __init__(self, address, port, scope, context):
        self.target = (address, port, 0, scope)
        super().__init__("airdrop", port, context=context, timeout=120)

    def connect(self):
        deadline = time.monotonic() + 25
        while True:
            raw = socket.socket(socket.AF_INET6)
            try:
                # HTTPSConnection normally disables Nagle; our scoped IPv6
                # connection must do it explicitly. BBR avoids treating every
                # missed AWDL channel window as congestion on this lossy link.
                raw.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
                try:
                    raw.setsockopt(socket.IPPROTO_TCP, socket.TCP_CONGESTION, b"bbr")
                except OSError:
                    pass  # Use the kernel default when BBR is unavailable.
                raw.settimeout(min(5, max(0.1, deadline - time.monotonic())))
                raw.connect(self.target)
                raw.settimeout(min(15, max(0.1, deadline - time.monotonic())))
                self.sock = self._context.wrap_socket(raw, server_hostname="airdrop")
                self.sock.settimeout(self.timeout)
                algorithm = self.sock.getsockopt(socket.IPPROTO_TCP, socket.TCP_CONGESTION, 16).rstrip(b"\0").decode()
                print(f"AirDrop TCP: {algorithm}", file=sys.stderr, flush=True)
                return
            except OSError as error:
                raw.close()
                # Retry only while establishing TCP/TLS, before any HTTP
                # request has been written. Never repeat an Ask or Upload.
                if error.errno not in (errno.ECONNREFUSED, errno.ECONNRESET, errno.ETIMEDOUT) and not isinstance(error, TimeoutError):
                    raise
                if time.monotonic() >= deadline:
                    raise RuntimeError("The iPhone is not accepting AirDrop connections. Enable Everyone for 10 Minutes, then open a photo → Share → AirDrop on the phone.") from error
                print("Waiting for iPhone AirDrop connection…", file=sys.stderr, flush=True)
                time.sleep(min(1, max(0, deadline - time.monotonic())))
            except BaseException:
                raw.close()
                raise


def send(address, files, discover=True):
    if len({path.name for path in files}) != len(files):
        raise ValueError("Choose files with different names, or send them separately.")
    host, port = address.rsplit("]:", 1)
    host = host.removeprefix("[").split("%", 1)[0]
    if not ipaddress.IPv6Address(host).is_link_local:
        raise ValueError("The recipient must be a nearby AirDrop device.")
    scope = socket.if_nametoindex("awdl0")
    prepare_state(STATE)
    with tempfile.TemporaryDirectory(prefix="sender-", dir=STATE) as temporary:
        # The radio supervisor checks both PID and process start time, so
        # an abandoned directory or reused PID cannot keep a session alive.
        started = Path(f"/proc/{os.getpid()}/stat").read_text().rsplit(")", 1)[1].split()[19]
        (Path(temporary) / "active.json").write_text(json.dumps({"pid": os.getpid(), "started": started}))
        cert, key = Path(temporary) / "cert.pem", Path(temporary) / "key.pem"
        subprocess.run(["openssl", "req", "-newkey", "rsa:2048", "-nodes", "-keyout", str(key),
                        "-x509", "-days", "1", "-out", str(cert), "-subj", "/CN=" + socket.gethostname()],
                       check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        context.check_hostname = False
        context.verify_mode = ssl.CERT_NONE
        context.load_cert_chain(cert, key)
        connection = ScopedHTTPS(host, int(port), scope, context)
        def post(path, body):
            if connection.sock is None:
                connection.connect()
            print("POST " + path, file=sys.stderr, flush=True)
            connection.request("POST", path, plistlib.dumps(body, fmt=plistlib.FMT_BINARY),
                               {"Content-Type": "application/octet-stream", "Connection": "keep-alive",
                                "User-Agent": "AirDrop/1.0"})
            response = connection.getresponse()
            response.read()
            if response.status != 200:
                raise RuntimeError("The iPhone declined the transfer." if path == "/Ask" else
                                   f"The iPhone returned {response.status} for {path}.")
        try:
            print("Preparing files…", file=sys.stderr, flush=True)
            with tempfile.TemporaryFile(dir=temporary) as payload:
                total = prepare_payload(files, payload)
                if discover:
                    post("/Discover", {})
                    connection.close()
                    connection = ScopedHTTPS(host, int(port), scope, context)
                transfer_id = str(uuid.uuid4()).upper()
                post("/Ask", {"SenderComputerName": socket.gethostname(), "BundleID": "com.apple.finder",
                              "SenderModelName": "OpenDrop", "SenderID": uuid.uuid4().hex[:12],
                              "ConvertMediaFormats": False, "TransferID": {"id": transfer_id},
                              "TransferType": {"files": {}}, "Files": [
                                  {"FileName": path.name, "FileType": uti(path), "FileBomPath": "./" + path.name,
                                   "FileIsDirectory": False, "ConvertMediaFormats": 0} for path in files]})
                if connection.sock is None:
                    raise RuntimeError("The iPhone closed the transfer connection after accepting. Please try again.")
                print("POST /Upload", file=sys.stderr, flush=True)
                headers = {"User-Agent": "AirDrop/1.0", "TotalBytes": str(total),
                           "Content-Type": "application/x-dvzip",
                           "SenderPseudonym": "pseud:" + base64.urlsafe_b64encode(os.urandom(16)).decode().rstrip("="),
                           "SenderPushToken": os.urandom(32).hex().upper(), "TransferID": transfer_id,
                           "Connection": "keep-alive", "Transfer-Encoding": "chunked"}
                connection.putrequest("POST", "/Upload", skip_host=True, skip_accept_encoding=True)
                for name, value in headers.items():
                    connection.putheader(name, value)
                connection.endheaders()
                sent = 0
                last_percent = -1
                while block := payload.read(16 * 1024):
                    connection.send(f"{len(block):x}\r\n".encode() + block + b"\r\n")
                    sent += len(block)
                    percent = min(99, sent * 100 // total)
                    if percent != last_percent:
                        print(f"Upload progress: {percent}", file=sys.stderr, flush=True)
                        last_percent = percent
                connection.send(b"0\r\n\r\n")
                response = connection.getresponse()
                response.read()
                if response.status != 200:
                    raise RuntimeError(f"The iPhone returned {response.status} after the upload.")
                print("Transfer completed.", file=sys.stderr, flush=True)
        finally:
            connection.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--address", required=True)
    parser.add_argument("--skip-discover", action="store_true", help="Recipient was already discovered by the sender window")
    parser.add_argument("files", type=Path, nargs="+")
    args = parser.parse_args()
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(130))
    try:
        send(args.address, args.files, discover=not args.skip_discover)
    except (OSError, ValueError, RuntimeError, subprocess.CalledProcessError, http.client.HTTPException) as error:
        print("Error: " + str(error), file=sys.stderr, flush=True)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
