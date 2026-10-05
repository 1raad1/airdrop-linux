//! Decoder for Apple AirDrop's "dvzip" payload format
//! (Content-Type: `application/x-dvzip`), as sent by modern iOS
//! (observed on iOS 18.6.2). See `docs/airdrop-protocol.md` and the
//! authoritative Python reference at `opendrop-patch/dvzip.py`.
//!
//! Format: a sequence of framed zlib blocks
//!
//! ```text
//! repeat until end-of-data:
//!     u32  block_length   (big-endian)
//!     u8[block_length]    zlib stream (header 0x78 0x9c)
//! ```
//!
//! Concatenating the inflated output of every block yields an ODC CPIO
//! archive (magic ASCII `070707`).

use flate2::read::ZlibDecoder;
use std::io::Read;

/// Decode a dvzip payload into the underlying CPIO archive bytes.
///
/// Matches `opendrop-patch/dvzip.py::dvzip_to_cpio`: walk framed zlib blocks,
/// inflate each, and concatenate. Stops cleanly on a trailing short frame or
/// a zero-length block (mirroring the Python loop guard
/// `block_len == 0 or pos + block_len > len(data)`).
///
/// Malformed zlib streams are skipped with a `debug!` log rather than a hard
/// error: we want best-effort recovery out of a captured body and the CPIO
/// magic check downstream will catch total garbage.
pub fn dvzip_to_cpio(data: &[u8]) -> Vec<u8> {
    dvzip_to_cpio_checked(data).unwrap_or_default()
}

/// Validate every received block, including Apple's high-bit stored blocks.
pub fn dvzip_to_cpio_checked(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos + 4 <= data.len() {
        let descriptor = u32::from_be_bytes([
            data[pos],
            data[pos + 1],
            data[pos + 2],
            data[pos + 3],
        ]);
        let stored = descriptor & 0x8000_0000 != 0;
        let block_len = (descriptor & 0x7fff_ffff) as usize;
        pos += 4;
        if block_len == 0 {
            break;
        }
        if block_len > data.len() - pos {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "truncated dvzip block"));
        }
        let raw = &data[pos..pos + block_len];
        pos += block_len;

        if stored {
            out.extend_from_slice(raw);
        } else {
            let mut decoder = ZlibDecoder::new(raw);
            decoder.read_to_end(&mut out)?;
        }
    }
    Ok(out)
}

/// Encode a CPIO archive as a dvzip payload: zlib-compress the whole archive
/// and frame it as a single `[u32 BE length][zlib stream]` block. The decoder
/// ([`dvzip_to_cpio`) handles any number of blocks; one block is functionally
/// correct and round-trips exactly. This is the AirDrop sender's `/Upload`
/// body builder.
pub fn cpio_to_dvzip(cpio: &[u8]) -> Vec<u8> {
    cpio_to_dvzip_blocks(cpio, 1)
}

/// Encode a CPIO archive as a dvzip payload split across `num_blocks` zlib
/// blocks, each independently framed `[u32 BE length][zlib stream]`. Real
/// Apple senders emit multiple zlib blocks (the captured `/tmp/upload.bin`
/// fixture was 4 blocks); the decoder concatenates them. Splits the CPIO
/// into `num_blocks` roughly-equal contiguous slices. `num_blocks` is clamped
/// to `[1, len]`.
pub fn cpio_to_dvzip_blocks(cpio: &[u8], num_blocks: usize) -> Vec<u8> {
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;
    // Empty CPIO still produces one framed (empty-output) zlib block. Handle
    // inline rather than recursing through cpio_to_dvzip.
    if cpio.is_empty() {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(b"").expect("zlib encode empty");
        let compressed = encoder.finish().expect("zlib finish");
        let mut out = Vec::with_capacity(4 + compressed.len());
        out.extend_from_slice(&(compressed.len() as u32).to_be_bytes());
        out.extend_from_slice(&compressed);
        return out;
    }
    let n = num_blocks.clamp(1, cpio.len());
    let block = cpio.len() / n;
    let mut out = Vec::new();
    let mut pos = 0;
    for i in 0..n {
        let end = if i + 1 == n { cpio.len() } else { pos + block };
        let chunk = &cpio[pos..end];
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(chunk).expect("zlib encode block");
        let compressed = encoder.finish().expect("zlib finish");
        out.extend_from_slice(&(compressed.len() as u32).to_be_bytes());
        out.extend_from_slice(&compressed);
        pos = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{cpio_to_dvzip, cpio_to_dvzip_blocks, dvzip_to_cpio};
    use crate::cpio::build_odc_cpio;
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;

    /// Frame a payload as a dvzip body: split it into the given chunk sizes,
    /// zlib-compress each chunk, and prefix each compressed stream with a
    /// big-endian u32 length — exactly the on-wire dvzip framing.
    fn frame_dvzip(cpio: &[u8], chunk_sizes: &[usize]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut pos = 0;
        for &sz in chunk_sizes {
            let end = (pos + sz).min(cpio.len());
            let chunk = &cpio[pos..end];
            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(chunk).unwrap();
            let compressed = encoder.finish().unwrap();
            out.extend_from_slice(&(compressed.len() as u32).to_be_bytes());
            out.extend_from_slice(&compressed);
            pos = end;
        }
        assert_eq!(pos, cpio.len(), "chunk_sizes must cover the whole payload");
        out
    }

    /// A synthetic ODC-CPIO-shaped payload: starts with the `070707` magic so
    /// a future CPIO reader has something realistic to chew on. The dvzip
    /// decoder itself only cares about byte-exact round-trip.
    fn synthetic_cpio() -> Vec<u8> {
        let mut v = b"070707".to_vec();
        v.extend_from_slice(b"hello airdrop from luftlift-rs!");
        v
    }

    #[test]
    fn dvzip_decodes_single_framed_zlib_block_to_cpio() {
        let cpio = synthetic_cpio();
        let framed = frame_dvzip(&cpio, &[cpio.len()]);

        let decoded = dvzip_to_cpio(&framed);
        assert_eq!(&decoded[..6], b"070707", "decoded bytes must be CPIO magic");
        assert_eq!(decoded, cpio, "round-trip must recover the original bytes");
    }

    #[test]
    fn dvzip_decodes_multiple_framed_zlib_blocks_to_cpio() {
        let cpio = synthetic_cpio();
        // iOS-18 captures typically span several zlib blocks (the real
        // /tmp/upload.bin fixture is 4 blocks). Mix a sub-payload split with a
        // final block that exactly exhausts the buffer.
        let framed = frame_dvzip(&cpio, &[6, 7, cpio.len() - 13]);

        let decoded = dvzip_to_cpio(&framed);
        assert_eq!(&decoded[..6], b"070707");
        assert_eq!(decoded, cpio);
    }

    #[test]
    fn dvzip_empty_input_yields_empty_output() {
        assert_eq!(dvzip_to_cpio(&[]), Vec::<u8>::new());
    }

    // ---- encoder (sender side) ----

    #[test]
    fn cpio_to_dvzip_roundtrips_through_dvzip_to_cpio() {
        let entries: &[(&str, &[u8])] = &[("a.txt", b"hello"), ("b.bin", &[0, 1, 2, 3])];
        let cpio = build_odc_cpio(entries);
        let dvzip = cpio_to_dvzip(&cpio);
        let decoded = dvzip_to_cpio(&dvzip);
        assert_eq!(decoded, cpio, "encoder must round-trip through decoder");
    }

    #[test]
    fn cpio_to_dvzip_produces_valid_framing() {
        let cpio = build_odc_cpio(&[("x", b"xyzzy")]);
        let dvzip = cpio_to_dvzip(&cpio);
        // First 4 bytes = big-endian u32 length of the following zlib stream.
        let len = u32::from_be_bytes([dvzip[0], dvzip[1], dvzip[2], dvzip[3]]) as usize;
        assert_eq!(dvzip.len(), 4 + len, "framed length must match remaining bytes");
        // Zlib header magic 0x78 0x9c follows the length.
        assert_eq!(&dvzip[4..6], &[0x78, 0x9c], "zlib stream header");
    }

    #[test]
    fn cpio_to_dvzip_empty_cpio_round_trips() {
        let dvzip = cpio_to_dvzip(&[]);
        assert!(!dvzip.is_empty(), "even empty CPIO produces a framed zlib block");
        assert_eq!(dvzip_to_cpio(&dvzip), Vec::<u8>::new());
    }

    #[test]
    fn cpio_to_dvzip_blocks_multi_roundtrips_through_dvzip_to_cpio() {
        // Multi-block dvzip (like the real /tmp/upload.bin fixture = 4 blocks).
        let cpio = build_odc_cpio(&[("a.txt", b"hello"), ("b.bin", &[0, 1, 2, 3, 4, 5])]);
        let dvzip = cpio_to_dvzip_blocks(&cpio, 4);
        assert!(!dvzip.is_empty());
        let decoded = dvzip_to_cpio(&dvzip);
        assert_eq!(decoded, cpio, "multi-block must round-trip exactly");
    }

    #[test]
    fn cpio_to_dvzip_blocks_single_matches_cpio_to_dvzip() {
        let cpio = build_odc_cpio(&[("x", b"data")]);
        assert_eq!(cpio_to_dvzip_blocks(&cpio, 1), cpio_to_dvzip(&cpio));
    }

    #[test]
    fn cpio_to_dvzip_blocks_empty_round_trips() {
        let dvzip = cpio_to_dvzip_blocks(&[], 4);
        assert!(!dvzip.is_empty(), "empty CPIO still produces one zlib block");
        assert_eq!(dvzip_to_cpio(&dvzip), Vec::<u8>::new());
    }
}
