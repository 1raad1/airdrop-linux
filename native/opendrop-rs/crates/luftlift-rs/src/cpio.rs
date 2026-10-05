//! Reader for the POSIX ODC CPIO archive format (old portable ASCII,
//! magic ASCII `070707`) — the inner format produced by decoding an
//! AirDrop dvzip payload (see `docs/airdrop-protocol.md` and the `dvzip`
//! module). Extracted entries are the transferred file(s); for a link
//! share the payload contains a `.webloc` file.
//!
//! ## ODC header layout (76 bytes, all ASCII octal)
//! ```text
//! offset  len  field
//! 0       6    c_magic     "070707"
//! 6       6    c_dev
//! 12      6    c_ino
//! 18      6    c_mode
//! 24      6    c_uid
//! 30      6    c_gid
//! 36      6    c_nlink
//! 42      6    c_rdev
//! 48      11   c_mtime
//! 59      6    c_namesize  (includes the trailing NUL)
//! 65      11   c_filesize
//! ```
//! The header is followed by `c_namesize` name bytes (NUL-terminated) and
//! then `c_filesize` data bytes — with **no** alignment padding in ODC
//! (unlike the "newc" `070701` format). The archive is terminated by an
//! entry named `TRAILER!!!` with a zero-length data body.

use tracing::debug;

/// A single file entry extracted from an ODC CPIO archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpioEntry<'a> {
    /// Entry name with the trailing NUL stripped.
    pub name: String,
    /// Raw file bytes, borrowed from the input archive.
    pub data: &'a [u8],
    /// The `c_mode` field (file type + permissions) from the header. The
    /// file-type bits (`mode & 0o170000`) distinguish regular files
    /// (`0o100000`) from directories (`0o040000`), symlinks, etc.
    pub mode: u32,
}

/// Fixed width of an ODC CPIO header (ASCII octal fields, no padding).
pub const HDR_LEN: usize = 76;
/// ODC magic bytes (ASCII `070707`).
pub const MAGIC: &[u8] = b"070707";

const NAMESIZE_OFF: usize = 59;
const NAMESIZE_LEN: usize = 6;
const FILESIZE_OFF: usize = 65;
const FILESIZE_LEN: usize = 11;
const MODE_OFF: usize = 18;
const MODE_LEN: usize = 6;
const TRAILER_NAME: &str = "TRAILER!!!";

/// `S_IFMT` mask for the file-type bits in a cpio `c_mode` field.
const S_IFMT: u32 = 0o170_000;
/// `S_IFDIR` — directory entry type.
const S_IFDIR: u32 = 0o040_000;

/// Require a complete, well-formed archive and at least one regular file.
/// Used before acknowledging an upload whose HTTP ending went missing.
pub fn odc_archive_complete(data: &[u8]) -> bool {
    let mut pos = 0usize;
    let mut regular = false;
    while let Some(header) = data.get(pos..pos.saturating_add(HDR_LEN)) {
        if !header.starts_with(MAGIC) { return false; }
        let octal = |field: &[u8]| -> Option<usize> {
            if !field.iter().all(|c| (b'0'..=b'7').contains(c)) { return None; }
            usize::from_str_radix(std::str::from_utf8(field).ok()?, 8).ok()
        };
        let (Some(namesize), Some(filesize), Some(mode)) = (
            octal(&header[59..65]), octal(&header[65..76]), octal(&header[18..24])
        ) else { return false; };
        pos += HDR_LEN;
        if namesize == 0 { return false; }
        let Some(end_name) = pos.checked_add(namesize) else { return false; };
        let Some(name) = data.get(pos..end_name) else { return false; };
        if name.last() != Some(&0) { return false; }
        let Some(end_file) = end_name.checked_add(filesize) else { return false; };
        if end_file > data.len() { return false; }
        if name == b"TRAILER!!!\0" { return filesize == 0 && regular; }
        regular |= mode & 0o170000 == 0o100000;
        pos = end_file;
    }
    false
}

/// Extract entries from an ODC CPIO archive (magic `070707`).
///
/// Walks 76-byte ASCII-octal headers, emitting each entry's name + data +
/// mode. Stops at the `TRAILER!!!` entry (which is not emitted) or at a short /
/// malformed frame. A missing or wrong magic on the first header yields no
/// entries — the downstream caller is expected to treat an empty result on
/// a non-empty input as "not a CPIO".
///
/// **Directory entries are skipped**: real Apple ODC CPIO archives lead with
/// a `"."` directory entry (mode `040755`) before the regular file(s), and
/// trying to save that as a file fails with `EISDIR`. Any entry whose mode
/// has `S_IFDIR` set (`mode & 0o170000 == 0o040000`) or whose name is `"."`
/// / `".."` is silently omitted from the result.
pub fn read_odc_cpio(data: &[u8]) -> Vec<CpioEntry<'_>> {
    let mut entries = Vec::new();
    let mut pos = 0;
    while pos + HDR_LEN <= data.len() {
        if &data[pos..pos + MAGIC.len()] != MAGIC {
            debug!(pos, "cpio: stopping, no ODC magic at header");
            break;
        }
        let mode = parse_octal(&data[pos + MODE_OFF..pos + MODE_OFF + MODE_LEN]) as u32;
        let namesize = parse_octal(&data[pos + NAMESIZE_OFF..pos + NAMESIZE_OFF + NAMESIZE_LEN]);
        let filesize = parse_octal(&data[pos + FILESIZE_OFF..pos + FILESIZE_OFF + FILESIZE_LEN]);
        pos += HDR_LEN;

        if namesize == 0 || pos + namesize > data.len() {
            debug!(pos, namesize, "cpio: stopping, short name field");
            break;
        }
        let name_bytes = &data[pos..pos + namesize];
        let name = core::str::from_utf8(name_bytes)
            .unwrap_or("")
            .trim_end_matches('\0')
            .to_string();
        pos += namesize;

        if pos + filesize > data.len() {
            debug!(pos, filesize, "cpio: stopping, short data field");
            break;
        }
        let file_data = &data[pos..pos + filesize];
        pos += filesize;

        if name == TRAILER_NAME {
            break;
        }

        // Skip directory entries (real Apple CPIO archives lead with a "."
        // directory entry; trying to save it as a file fails with EISDIR).
        // Also defensively skip "." / ".." regardless of mode bits.
        if (mode & S_IFMT) == S_IFDIR || name == "." || name == ".." {
            debug!(name = %name, mode = format!("{:06o}", mode), "cpio: skipping directory entry");
            continue;
        }

        entries.push(CpioEntry { name, data: file_data, mode });
    }
    entries
}

/// Parse a fixed-width ASCII-octal field, tolerating trailing NUL/space.
fn parse_octal(field: &[u8]) -> usize {
    let s = match core::str::from_utf8(field) {
        Ok(s) => s,
        Err(_) => return 0,
    };
    let trimmed = s.trim_matches(|c: char| c.is_ascii_whitespace() || c == '\0');
    usize::from_str_radix(trimmed, 8).unwrap_or(0)
}

// ---- Encoder (sender side) ----

/// Build a single 76-byte ODC header with an explicit `c_mode` (file type +
/// permission bits). Used internally by the encoder (regular file 100644) and
/// by tests to craft directory entries (040755).
fn build_header_with_mode(ino: u32, namesize: usize, filesize: usize, mode: u32) -> String {
    let mut s = String::new();
    s.push_str("070707"); // c_magic
    s.push_str("000000"); // c_dev
    s.push_str(&format!("{:06o}", ino & 0o777777)); // c_ino
    s.push_str(&format!("{:06o}", mode & 0o777_777)); // c_mode
    s.push_str("000000"); // c_uid
    s.push_str("000000"); // c_gid
    s.push_str("000001"); // c_nlink
    s.push_str("000000"); // c_rdev
    s.push_str("00000000000"); // c_mtime (11 octal digits)
    s.push_str(&format!("{:06o}", namesize)); // c_namesize
    s.push_str(&format!("{:011o}", filesize)); // c_filesize
    debug_assert_eq!(s.len(), HDR_LEN, "ODC header must be exactly 76 bytes");
    s
}

/// Encode a sequence of `(name, data)` entries as an ODC CPIO archive,
/// terminated by the `TRAILER!!!` entry. This is the inverse of
/// [`read_odc_cpio`] and the inner payload the AirDrop sender frames as
/// dvzip for `/Upload`. Does NOT prepend a leading "." directory entry —
/// the luftlift sender produces plain regular-file archives.
pub fn build_odc_cpio(entries: &[(&str, &[u8])]) -> Vec<u8> {
    build_odc_cpio_with_leading_dir(entries, false)
}

/// Like [`build_odc_cpio`] but optionally prepends a `"."` directory entry
/// (mode `040755`, 0 bytes) — the shape a real Apple sender produces. When
/// `include_dot_dir` is false this is identical to `build_odc_cpio`. The
/// leading directory entry is filtered out by [`read_odc_cpio`]; it exists
/// here so tests can reproduce the real on-wire archive byte shape.
pub fn build_odc_cpio_with_leading_dir(entries: &[(&str, &[u8])], include_dot_dir: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let mut ino = 1u32;
    if include_dot_dir {
        let dot_name = ".\0";
        let header = build_header_with_mode(ino, dot_name.len(), 0, 0o040_755);
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(dot_name.as_bytes());
        ino += 1;
    }
    for &(name, data) in entries.iter() {
        let name_with_nul = format!("{}\0", name);
        let header = build_header_with_mode(ino, name_with_nul.len(), data.len(), 0o100_644);
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(name_with_nul.as_bytes());
        out.extend_from_slice(data);
        ino += 1;
    }
    let trailer = "TRAILER!!!\0";
    let header = build_header_with_mode(0, trailer.len(), 0, 0);
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(trailer.as_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::{
        CpioEntry, HDR_LEN, MAGIC, S_IFMT, build_header_with_mode, build_odc_cpio,
        read_odc_cpio,
    };

    /// Build a single 76-byte ODC header as ASCII. Asserts its own length so
    /// a field-offset regression in the test scaffolding is caught loudly.
    fn build_header(ino: u32, namesize: usize, filesize: usize) -> String {
        build_header_with_mode(ino, namesize, filesize, 0o100_644)
    }

    /// Encode a sequence of (name, data) entries as an ODC CPIO archive,
    /// optionally appending the `TRAILER!!!` terminator real writers emit.
    fn encode_odc_cpio(entries: &[(&str, &[u8])], with_trailer: bool) -> Vec<u8> {
        let mut out = Vec::new();
        for (i, &(name, data)) in entries.iter().enumerate() {
            let name_with_nul = format!("{}\0", name);
            let header = build_header((i + 1) as u32, name_with_nul.len(), data.len());
            out.extend_from_slice(header.as_bytes());
            out.extend_from_slice(name_with_nul.as_bytes());
            out.extend_from_slice(data);
        }
        if with_trailer {
            let trailer = "TRAILER!!!\0";
            let header = build_header(0, trailer.len(), 0);
            out.extend_from_slice(header.as_bytes());
            out.extend_from_slice(trailer.as_bytes());
        }
        out
    }

    #[test]
    fn read_odc_cpio_decodes_single_handbuilt_entry() {
        // Ground-truth archive built without the encoder: one entry "a" with
        // empty data. Header is exactly 76 ASCII bytes; then "a\0".
        let mut archive = Vec::new();
        archive.extend_from_slice(build_header(1, 2, 0).as_bytes()); // namesize=2 ("a\0")
        archive.extend_from_slice(b"a\0");
        assert_eq!(archive.len(), HDR_LEN + 2);

        let entries = read_odc_cpio(&archive);
        assert_eq!(
            entries,
            vec![CpioEntry {
                name: "a".to_string(),
                data: b"",
                mode: 0o100_644,
            }]
        );
    }

    #[test]
    fn read_odc_cpio_roundtrips_multiple_entries() {
        let jpeg_ish = [0xFFu8, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46];
        let entries_in: &[(&str, &[u8])] = &[
            ("hello.txt", b"hi from luftlift"),
            ("FullSizeRender.jpg", &jpeg_ish),
            ("dir/nested.bin", &[0x00, 0xFF, 0x7F, 0x80, 0x01]),
        ];
        let archive = encode_odc_cpio(entries_in, true);

        let got = read_odc_cpio(&archive);
        assert_eq!(got.len(), entries_in.len());
        for (expected, actual) in entries_in.iter().zip(got.iter()) {
            assert_eq!(actual.name, expected.0);
            assert_eq!(actual.data, expected.1);
        }
    }

    #[test]
    fn read_odc_cpio_stops_at_trailer_and_omits_it() {
        let entries_in: &[(&str, &[u8])] = &[("only.txt", b"x")];
        let archive = encode_odc_cpio(entries_in, true);

        let got = read_odc_cpio(&archive);
        assert_eq!(got.len(), 1, "TRAILER!!! must not appear as an entry");
        assert_eq!(got[0].name, "only.txt");
        assert_eq!(got[0].data, b"x");
    }

    #[test]
    fn read_odc_cpio_preserves_exact_binary_bytes() {
        // A byte sweep including the tricky values (0x00, 0xFF, 0x0A newline,
        // 0x00 NUL inside *data* — distinct from name NULs).
        let data: Vec<u8> = (0u8..=255).collect();
        let archive = encode_odc_cpio(&[("sweep.bin", &data)], true);

        let got = read_odc_cpio(&archive);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].data, data.as_slice());
    }

    #[test]
    fn read_odc_cpio_empty_archive_yields_no_entries() {
        assert!(read_odc_cpio(&[]).is_empty());
    }

    #[test]
    fn read_odc_cpio_non_magic_input_yields_no_entries() {
        // Bytes that are not an ODC CPIO: must not be misread as one.
        let bogus = b"this is definitely not a cpio archive at all";
        assert!(read_odc_cpio(bogus).is_empty());
        // Sanity: the magic constant is what we expect on the wire.
        assert_eq!(MAGIC, b"070707");
    }

    // ---- public encoder round-trips (sender side) ----

    #[test]
    fn build_odc_cpio_roundtrips_through_read_odc_cpio() {
        let data: Vec<u8> = (0u8..=255).collect();
        let entries: &[(&str, &[u8])] = &[
            ("photo.jpg", &data),
            ("hello.txt", b"hi"),
            ("dir/nested.bin", &[0x00, 0xFF, 0x7F]),
        ];
        let archive = build_odc_cpio(entries);
        assert_eq!(&archive[..6], MAGIC, "archive starts with ODC magic");

        let decoded = read_odc_cpio(&archive);
        assert_eq!(decoded.len(), entries.len());
        for (expected, actual) in entries.iter().zip(decoded.iter()) {
            assert_eq!(actual.name, expected.0);
            assert_eq!(actual.data, expected.1);
        }
    }

    #[test]
    fn build_odc_cpio_empty_entries_yields_only_trailer() {
        let archive = build_odc_cpio(&[]);
        // Only the TRAILER!!! header+name — read_odc_cpio stops at it, no entries.
        assert_eq!(archive.len(), HDR_LEN + "TRAILER!!!\0".len());
        assert!(read_odc_cpio(&archive).is_empty());
    }

    #[test]
    fn build_odc_cpio_single_entry_exact_bytes() {
        // Byte-exact check for a known-small archive: one entry "a" + empty data.
        let archive = build_odc_cpio(&[("a", b"")]);
        // header(76) + "a\0"(2) + trailer header(76) + "TRAILER!!!\0"(11)
        assert_eq!(archive.len(), HDR_LEN + 2 + HDR_LEN + 11);
        let decoded = read_odc_cpio(&archive);
        assert_eq!(decoded, vec![CpioEntry { name: "a".to_string(), data: b"", mode: 0o100_644 }]);
    }

    // ---- directory entry filtering (the iPhone "." bug) ----

    /// Build an ODC CPIO archive with a leading "." directory entry (mode
    /// 040755, 0 bytes) followed by regular file entries — the shape a real
    /// Apple sender produces. Returns the raw archive bytes.
    fn build_apple_style_cpio(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        // Leading "." directory entry (mode 040755).
        let dot_name = ".\0";
        let dot_hdr = build_header_with_mode(1, dot_name.len(), 0, 0o040_755);
        out.extend_from_slice(dot_hdr.as_bytes());
        out.extend_from_slice(dot_name.as_bytes());
        // Regular file entries.
        for (i, &(name, data)) in files.iter().enumerate() {
            let name_with_nul = format!("{}\0", name);
            let header = build_header_with_mode(
                (i + 2) as u32,
                name_with_nul.len(),
                data.len(),
                0o100_644,
            );
            out.extend_from_slice(header.as_bytes());
            out.extend_from_slice(name_with_nul.as_bytes());
            out.extend_from_slice(data);
        }
        // Trailer.
        let trailer = "TRAILER!!!\0";
        let header = build_header_with_mode(0, trailer.len(), 0, 0);
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(trailer.as_bytes());
        out
    }

    #[test]
    fn read_odc_cpio_skips_leading_dot_directory_entry() {
        // Real Apple CPIO: leading "." dir (040755) + a real file.
        let archive = build_apple_style_cpio(&[("photo.jpg", b"JPEG_DATA")]);
        let entries = read_odc_cpio(&archive);
        // The "." directory entry must NOT appear — only the real file.
        assert_eq!(entries.len(), 1, "directory entry must be skipped");
        assert_eq!(entries[0].name, "photo.jpg");
        assert_eq!(entries[0].data, b"JPEG_DATA");
        assert_eq!(entries[0].mode & S_IFMT, 0o100_000, "mode must be regular file");
    }

    #[test]
    fn read_odc_cpio_skips_dot_dot_directory_entry() {
        // Defensively skip ".." even if mode bits are wrong.
        let mut out = Vec::new();
        let name = "..\0";
        let hdr = build_header_with_mode(1, name.len(), 0, 0o100_644);
        out.extend_from_slice(hdr.as_bytes());
        out.extend_from_slice(name.as_bytes());
        let file_name = "real.txt\0";
        let hdr2 = build_header_with_mode(2, file_name.len(), 3, 0o100_644);
        out.extend_from_slice(hdr2.as_bytes());
        out.extend_from_slice(file_name.as_bytes());
        out.extend_from_slice(b"abc");
        let trailer = "TRAILER!!!\0";
        let hdr3 = build_header_with_mode(0, trailer.len(), 0, 0);
        out.extend_from_slice(hdr3.as_bytes());
        out.extend_from_slice(trailer.as_bytes());

        let entries = read_odc_cpio(&out);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "real.txt");
    }

    #[test]
    fn read_odc_cpio_parses_mode_field() {
        // Verify the mode field is parsed and exposed on CpioEntry.
        let archive = build_odc_cpio(&[("file.txt", b"hi")]);
        let entries = read_odc_cpio(&archive);
        assert_eq!(entries[0].mode, 0o100_644, "regular-file mode must be parsed");
    }
}
