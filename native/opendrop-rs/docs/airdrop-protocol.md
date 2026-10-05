# AirDrop protocol notes (reverse-engineered, iOS 18.6.2)

Captured from a real iPhone 15 Pro Max (iOS 18.6.2) and a MacBook (macOS 15.7.3)
AirDropping to the patched Python opendrop. This is ground truth for `luftlift-rs`.

## Transport
- AWDL link (`awdl0`), IPv6 link-local. mDNS `_airdrop._tcp`, HTTPS on port 8771.
- Apple devices announce `_airdrop._tcp` only while their **sharing pane is open**
  (sender picker open, or AirDrop receive window). A device set to "Everyone" with
  a closed pane is not continuously discoverable by us (we can't send the BLE wake).

## HTTP flow
`POST /Discover` → `POST /Ask` → `POST /Upload` (Upload absent for some link sends
where the item rides in the Ask/Upload as a tiny webloc).

### iOS-18 quirks (these broke stock opendrop)
1. `/Discover` and `/Ask` bodies use **`Transfer-Encoding: chunked`** with **no
   `Content-Length`**. Read the chunked body; never assume Content-Length exists.
2. Upload Content-Type is **`application/x-dvzip`** on iOS 18 (older: `application/x-cpio`).

### /Discover
Request body: sender record plist. Response: our record plist, e.g.
`{ ReceiverComputerName, ReceiverModelName, ReceiverMediaCapabilities }`.
Responding makes us appear in the sender's picker.

### /Ask
Request body (binary plist) describes the transfer. Real captured example (a
Safari link share):
```
TransferID:          <uuid>
TransferType:        links
SenderID:            <hex>
SenderComputerName:  iPhone
SenderModelName:     iPhone
BundleID:            com.apple.mobilesafari
Files: [ { FileName: "bsky-app.webloc",
           FileType: "com.apple.web-internet-location",
           FileSize: 83,
           FileBomPath: "./bsky-app.webloc" } ]
Items: [ ]
FileIcon:            <png bytes>
SenderRecordData:    <plist with the sender's Apple ID cert; contains ocsp/crl
                      apple.com URLs — those are CERT noise, not the shared link>
```
A **link** is just a `.webloc` FILE (`com.apple.web-internet-location`); the URL is
inside the webloc, delivered in the Upload (or, for some sends, the webloc is the
payload). Don't look for a "URL" key — parse the webloc.
Respond 200 with `{ ReceiverModelName, ReceiverComputerName }` to auto-accept.

### /Upload
Body = the file archive. Content-Type `x-dvzip` (iOS 18) or `x-cpio` (legacy),
chunked. Decode (see dvzip below) → CPIO → extract files.

## dvzip format (Content-Type: application/x-dvzip)
A sequence of framed zlib blocks:
```
repeat until end:
    u32  block_length   (big-endian)
    u8[block_length]    zlib stream (header 0x78 0x9c)
```
Inflate every block and concatenate → an **ODC CPIO** archive (magic ASCII
`070707`). Extract the CPIO entries to get the file(s).

Worked example (real capture, `/tmp/upload.bin`):
- 314888 bytes → 4 zlib blocks → 317952 bytes CPIO → `FullSizeRender.jpg` (317435 B).
- First 8 bytes: `00 00 00 5a 78 9c 33 30` → length 0x5a=90, then zlib `78 9c`.

Reference decoder (authoritative): `../opendrop-patch/dvzip.py`. Save
`/tmp/upload.bin` as a test fixture and assert byte-exact JPEG extraction.

## TLS
Self-signed cert is accepted by Apple for AirDrop. opendrop generates one at
startup. Use rustls; don't require client-cert validation for `receive`.
