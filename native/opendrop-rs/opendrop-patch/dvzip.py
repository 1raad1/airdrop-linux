#!/usr/bin/env python3
"""
Decoder for Apple AirDrop's "dvzip" payload format (Content-Type:
application/x-dvzip), as sent by modern iOS (observed on iOS 18.6.2).

Reverse-engineered 2026-06-25 from a real iPhone 15 Pro Max transfer.

Format
------
A dvzip body is a sequence of framed zlib blocks:

    repeat until end-of-data:
        uint32  block_length   (big-endian)
        bytes   zlib_stream[block_length]   (zlib/deflate, header 0x78 0x9c)

Concatenating the inflated output of every block yields a CPIO archive in the
old portable-ASCII ("odc", magic b"070707") format, which contains the
transferred file(s). libarchive reads that directly.

Older iOS/macOS instead send Content-Type application/x-cpio with a (possibly
gzipped) CPIO body and no framing.
"""
import struct
import zlib


def dvzip_to_cpio(data: bytes) -> bytes:
    """Decode a dvzip payload into the underlying CPIO archive bytes."""
    out = bytearray()
    pos = 0
    while pos + 4 <= len(data):
        (block_len,) = struct.unpack(">I", data[pos:pos + 4])
        pos += 4
        if block_len == 0 or pos + block_len > len(data):
            break
        out += zlib.decompress(data[pos:pos + block_len])
        pos += block_len
    return bytes(out)


if __name__ == "__main__":
    import sys

    if len(sys.argv) != 2:
        print("usage: dvzip.py <upload.bin>  -> writes <upload.bin>.cpio")
        sys.exit(1)
    raw = open(sys.argv[1], "rb").read()
    cpio = dvzip_to_cpio(raw)
    assert cpio[:6] == b"070707", f"unexpected magic {cpio[:6]!r}"
    out = sys.argv[1] + ".cpio"
    open(out, "wb").write(cpio)
    print(f"{len(raw)} bytes dvzip -> {len(cpio)} bytes cpio, wrote {out}")
