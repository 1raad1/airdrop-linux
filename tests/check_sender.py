"""Exercise the packaged sender against an IPv6 TLS protocol fixture."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import importlib.util
import sys
import os
from pathlib import Path
import plistlib
import socket
import ssl
import subprocess
import tempfile
import threading
from unittest.mock import patch
import zlib

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "AirDropReceiver"))
base = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("sender", base / "AirDropReceiver/send-engine.py")
sender = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sender)

def decode(payload):
    archive = bytearray()
    while payload:
        descriptor = int.from_bytes(payload[:4], "big")
        size = descriptor & 0x7fffffff
        block, payload = payload[4:4 + size], payload[4 + size:]
        archive.extend(block if descriptor >> 31 else zlib.decompress(block))
    files = {}
    while archive:
        assert archive[:6] == b"070707"
        namesize, size = int(archive[59:65], 8), int(archive[65:76], 8)
        name = os.fsdecode(bytes(archive[76:76 + namesize - 1]))
        data = bytes(archive[76 + namesize:76 + namesize + size])
        del archive[:76 + namesize + size]
        if name == "TRAILER!!!":
            assert not archive
            break
        if name != ".":
            files[name] = data
    return files

class Server(ThreadingHTTPServer):
    address_family = socket.AF_INET6
    daemon_threads = False

class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *_):
        pass
    def do_POST(self):
        assert self.connection.getpeercert(), "Sender did not supply its TLS identity"
        if self.headers.get("Transfer-Encoding") == "chunked":
            body = bytearray()
            while size := int(self.rfile.readline().strip(), 16):
                body.extend(self.rfile.read(size))
                assert self.rfile.read(2) == b"\r\n"
            assert self.rfile.readline() == b"\r\n"
            body = bytes(body)
        else:
            body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        self.server.requests.append((self.path, dict(self.headers), body, self.connection))
        status = 403 if self.server.decline and self.path == "/Ask" else 200
        reply = plistlib.dumps({"ReceiverComputerName": "Fixture iPhone"}, fmt=plistlib.FMT_BINARY)
        self.send_response(status)
        # The real iPhone uses chunked binary-plist replies.
        self.send_header("Transfer-Encoding", "chunked")
        self.end_headers()
        self.wfile.write(f"{len(reply):x}\r\n".encode() + reply + b"\r\n0\r\n\r\n")
        self.wfile.flush()

with tempfile.TemporaryDirectory(prefix="AirDrop sender test ") as temporary:
    root = Path(temporary)
    sender.STATE = root / "sender-state"
    cert, key = root / "cert.pem", root / "key.pem"
    subprocess.run(["openssl", "req", "-newkey", "rsa:2048", "-nodes", "-keyout", str(key),
                    "-x509", "-days", "1", "-out", str(cert), "-subj", "/CN=fixture"],
                   check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    server = Server(("::1", 0), Handler)
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(cert, key)
    server.socket = tls.wrap_socket(server.socket, server_side=True)
    server.requests, server.decline = [], False
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    original_init = sender.ScopedHTTPS.__init__
    def fixture_init(self, host, port, scope, context):
        tls.load_verify_locations(next(sender.STATE.glob("sender-*/cert.pem")))
        tls.verify_mode = ssl.CERT_OPTIONAL
        original_init(self, "::1", server.server_port, 0, context)
    expected = {"./photo with spaces.txt": b"example photo description\n",
                "./vidéo.bin": os.urandom(400000)}
    paths = []
    for name, data in expected.items():
        path = root / name.removeprefix("./")
        path.write_bytes(data)
        paths.append(path)
    try:
        with patch.object(sender.ScopedHTTPS, "__init__", fixture_init), patch.object(sender.socket, "if_nametoindex", return_value=42):
            sender.send("[fe80::1234%999]:8770", paths)
            discover, ask, upload = server.requests
            assert [request[0] for request in server.requests] == ["/Discover", "/Ask", "/Upload"]
            assert plistlib.loads(discover[2]) == {}
            request = plistlib.loads(ask[2])
            assert request["SenderComputerName"] == socket.gethostname()
            assert isinstance(request["SenderID"], str) and len(request["SenderID"]) == 12
            assert request["TransferType"] == {"files": {}}
            assert "Items" not in request
            assert all(not item["FileIsDirectory"] for item in request["Files"])
            assert ask[3] is upload[3] and discover[3] is not ask[3]
            assert upload[1]["TransferID"] == request["TransferID"]["id"]
            assert int(upload[1]["TotalBytes"]) == len(upload[2])
            assert decode(upload[2]) == expected
            print("TLS, chunked replies, same-connection Ask/Upload, structured transfer ID, multi-file Unicode/spaces and stored/compressed blocks: PASS")
            server.requests.clear()
            sender.send("[fe80::1234%999]:8770", paths, discover=False)
            assert [request[0] for request in server.requests] == ["/Ask", "/Upload"]
            assert server.requests[0][3] is server.requests[1][3]
            assert decode(server.requests[1][2]) == expected
            print("Already-discovered recipient uses a single Ask/Upload connection without an extra Discover: PASS")
            server.decline = True
            server.requests.clear()
            try:
                sender.send("[fe80::1234%999]:8770", paths)
                raise AssertionError("Decline was reported as success")
            except RuntimeError as error:
                assert "declined" in str(error)
            assert [request[0] for request in server.requests] == ["/Discover", "/Ask"]
            assert not list(sender.STATE.glob("sender-*"))
            print("Declined transfer does not upload; temporary sender keys and archives removed: PASS")
    finally:
        server.shutdown()
        server.server_close()
    # Real ECONNREFUSED first, then start the same endpoint. Only the
    # connection establishment is retried, with no HTTP requests repeated.
    class ConnectionOnly(Handler):
        def handle(self):
            try:
                self.connection.recv(1)
            except OSError:
                pass
    delayed = Server(("::1", 0), ConnectionOnly, bind_and_activate=False)
    delayed.server_bind()
    delayed.requests, delayed.decline = [], False
    delayed.socket = tls.wrap_socket(delayed.socket, server_side=True)
    def start_delayed():
        delayed.server_activate()
        threading.Thread(target=delayed.serve_forever, daemon=True).start()
    timer = threading.Timer(0.8, start_delayed)
    timer.start()
    client_tls = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    client_tls.check_hostname = False
    client_tls.verify_mode = ssl.CERT_NONE
    connection = sender.ScopedHTTPS("::1", delayed.server_address[1], 0, client_tls)
    try:
        connection.connect()
        assert connection.sock is not None and not delayed.requests
        assert connection.sock.getsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY) == 1
        available = Path("/proc/sys/net/ipv4/tcp_available_congestion_control").read_text().split()
        if "bbr" in available:
            assert connection.sock.getsockopt(socket.IPPROTO_TCP, socket.TCP_CONGESTION, 16).rstrip(b"\0") == b"bbr"
        print("Scoped TLS socket disables Nagle and selects BBR when available: PASS")
        print("Transient TCP refusal recovers when receiver starts; no Ask or Upload is repeated: PASS")
    finally:
        connection.close()
        timer.join()
        delayed.shutdown()
        delayed.server_close()
