# opendrop-patch — modern-iOS (iOS 18) compatibility fixes for OpenDrop receive

Part of the opendrop-rs project. These patches make the Python
[OpenDrop](https://github.com/seemoo-lab/opendrop) `receive` path work with
**current iOS** (verified receiving a photo from an **iPhone 15 Pro Max on iOS
18.6.2**). Stock OpenDrop (last released ~2021) fails against modern iOS at
three points. `server.py.patched` is the working `opendrop/server.py`.

## Fixes

1. **Chunked request bodies (no Content-Length).**
   Modern iOS sends `/Discover` and `/Ask` POSTs with `Transfer-Encoding:
   chunked` and no `Content-Length`. Stock code does
   `int(self.headers["Content-Length"])` → `TypeError: int(None)` and the
   request crashes, so the receiver never appears on the phone. Added
   `_read_request_body()` which reads either a Content-Length body or a chunked
   body. → **the Linux box now shows up in the iPhone's AirDrop sheet.**

2. **`application/x-dvzip` upload Content-Type.**
   Stock code only accepts `application/x-cpio` for `/Upload` and rejects the
   modern payload with "Unsupported content-type". Now accepts both.

3. **The dvzip payload format itself.**
   Newer iOS wraps the CPIO archive as "dvzip" — a sequence of framed zlib
   blocks (`[uint32 big-endian length][zlib stream]…`) that inflate+concatenate
   into an ODC CPIO. libarchive can't read that directly ("Unrecognized archive
   format"). `handle_upload` now detects `x-dvzip` and decodes it back to CPIO
   before extraction. Standalone decoder + format notes: `dvzip.py`.

With all three, a real iOS 18 → Linux AirDrop transfer completes end to end:
```
File(s) received (size 0.30 MB, speed 2.17 MB/s)
FullSizeRender.jpg: JPEG ... Apple iPhone 15 Pro Max, software=18.6.2, 1244x1838
```

## Radio note

This only matters once the AWDL link is solid. The Realtek RTL8822BU clones gave
too much sync jitter for discovery to complete; an **Atheros AR9170 / carl9170**
USB adapter (`-N -c 44`) brought owl's sync to ~0.4% out-of-threshold and the
phone immediately discovered the receiver. See the owl
`../owl/src/rx.c` hardware-TSF patch for the Realtek work. Discovery is still
intermittent when multiple Apple devices nearby compete as AWDL master.

## Install

```bash
cp server.py.patched "$(python3 -c 'import opendrop,os;print(os.path.dirname(opendrop.__file__))')/server.py"
# then, as root, with owl running on awdl0:
python3 -m opendrop -i awdl0 -n linuxdrop receive
```
