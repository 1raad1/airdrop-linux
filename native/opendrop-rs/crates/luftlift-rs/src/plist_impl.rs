//! AirDrop plist (de)serialisation for `/Discover` and `/Ask` request/response
//! bodies. Apple uses **binary** plists (`bplist00`) on the wire. See
//! `docs/airdrop-protocol.md` for the field shapes and
//! `opendrop-patch/server.py.patched` (`handle_discover`, `handle_ask`) for the
//! reference behaviour.
//!
//! Key field details (from a real iOS-18.6.2 capture):
//! - **Discover response**: `ReceiverComputerName`, `ReceiverModelName`,
//!   `ReceiverMediaCapabilities` (a JSON blob `{"Version":1}` stored as raw
//!   bytes), and optionally `ReceiverRecordData`.
//! - **Ask request**: `TransferID`, `TransferType`, `SenderID`, bundle etc.,
//!   plus `Files: [{FileName,FileType,FileSize,FileBomPath}]`.
//! - **Ask response**: `ReceiverComputerName`, `ReceiverModelName`.

use plist::{Dictionary, Value};
use std::io::Cursor;
use thiserror::Error;

/// Receiver identity/configuration used to build responses.
#[derive(Debug, Clone)]
pub struct ReceiverConfig {
    pub computer_name: String,
    pub computer_model: String,
    /// Optional receiver record (Apple ID cert). Most non-Apple receivers omit this.
    pub record_data: Option<Vec<u8>>,
}

/// Fields we care about from a `/Ask` request body.
#[derive(Debug, Clone, PartialEq)]
pub struct AskRequest {
    pub transfer_id: String,
    pub transfer_type: String,
    pub sender_computer_name: String,
    pub sender_model_name: String,
    pub bundle_id: String,
    pub files: Vec<AskFile>,
}

/// One file descriptor inside an Ask request's `Files` array.
#[derive(Debug, Clone, PartialEq)]
pub struct AskFile {
    pub file_name: String,
    pub file_type: String,
    pub file_size: u64,
    pub file_bom_path: String,
}

#[derive(Debug, Error)]
pub enum PlistError {
    #[error("plist parse failed: {0}")]
    Parse(String),
    #[error("missing required field: {0}")]
    MissingField(String),
    #[error("unexpected type for field {field}: expected {expected}")]
    TypeError { field: String, expected: &'static str },
}

// ---- RED stubs --------------------------------------------------------------

/// Build a `/Discover` response plist (binary) for the given receiver.
///
/// Mirrors `server.py.patched::handle_discover`: includes
/// `ReceiverComputerName`, `ReceiverModelName`, `ReceiverMediaCapabilities`
/// (a `{"Version":1}` JSON blob stored as raw data), and optionally
/// `ReceiverRecordData`.
pub fn build_discover_response(cfg: &ReceiverConfig) -> Vec<u8> {
    // Match Python's json.dumps({"Version": 1}) byte-for-byte: default
    // separators (", ", ": ") put a space after the colon -> {"Version": 1}.
    // A real Apple sender parses this as JSON so the whitespace is functionally
    // inert, but we match the reference exactly.
    let caps_json = br#"{"Version": 1}"#;
    let mut dict = Dictionary::new();
    dict.insert("ReceiverMediaCapabilities".into(), Value::Data(caps_json.to_vec()));
    dict.insert("ReceiverComputerName".into(), Value::String(cfg.computer_name.clone()));
    dict.insert("ReceiverModelName".into(), Value::String(cfg.computer_model.clone()));
    if let Some(rec) = &cfg.record_data {
        dict.insert("ReceiverRecordData".into(), Value::Data(rec.clone()));
    }
    let mut buf = Vec::new();
    Value::Dictionary(dict)
        .to_writer_binary(&mut buf)
        .expect("serialise discover response");
    buf
}

/// Build an `/Ask` response plist (binary) for the given receiver.
pub fn build_ask_response(cfg: &ReceiverConfig) -> Vec<u8> {
    let mut dict = Dictionary::new();
    dict.insert("ReceiverModelName".into(), Value::String(cfg.computer_model.clone()));
    dict.insert("ReceiverComputerName".into(), Value::String(cfg.computer_name.clone()));
    let mut buf = Vec::new();
    Value::Dictionary(dict)
        .to_writer_binary(&mut buf)
        .expect("serialise ask response");
    buf
}

/// Parse a `/Ask` request body (binary or XML plist) into structured fields.
pub fn parse_ask_request(body: &[u8]) -> Result<AskRequest, PlistError> {
    let val = Value::from_reader(Cursor::new(body)).map_err(|e| PlistError::Parse(e.to_string()))?;
    let dict = val
        .as_dictionary()
        .ok_or(PlistError::TypeError { field: "root".into(), expected: "dictionary" })?;

    // Real iOS senders do NOT include TransferID in the /Ask body (the python
    // opendrop reference requires no fields at all here — it just accepts and
    // responds with the receiver name/model). Keep every field optional so a
    // missing one never rejects a genuine transfer.
    let transfer_id = get_string(dict, "TransferID").unwrap_or_default();
    let transfer_type = get_string(dict, "TransferType").unwrap_or_default();
    let sender_computer_name = get_string(dict, "SenderComputerName").unwrap_or_default();
    let sender_model_name = get_string(dict, "SenderModelName").unwrap_or_default();
    let bundle_id = get_string(dict, "BundleID").unwrap_or_default();

    let mut files = Vec::new();
    if let Some(Value::Array(arr)) = dict.get("Files") {
        for entry in arr {
            if let Value::Dictionary(f) = entry {
                files.push(AskFile {
                    file_name: get_string(f, "FileName").unwrap_or_default(),
                    file_type: get_string(f, "FileType").unwrap_or_default(),
                    file_size: f
                        .get("FileSize")
                        .and_then(|v| v.as_signed_integer())
                        .map(|i| i as u64)
                        .unwrap_or(0),
                    file_bom_path: get_string(f, "FileBomPath").unwrap_or_default(),
                });
            }
        }
    }

    Ok(AskRequest {
        transfer_id,
        transfer_type,
        sender_computer_name,
        sender_model_name,
        bundle_id,
        files,
    })
}

fn get_string(dict: &Dictionary, key: &str) -> Result<String, PlistError> {
    dict.get(key)
        .and_then(|v| v.as_string().map(|s| s.to_string()))
        .ok_or_else(|| PlistError::MissingField(key.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use plist::Dictionary;
    use plist::Value;

    // ---- helpers to build plists the way iOS does ----

    fn make_ask_request_plist() -> Vec<u8> {
        // Mirrors the real captured Ask body from docs/airdrop-protocol.md
        // (a Safari link share with a single .webloc file).
        let mut files_entry = Dictionary::new();
        files_entry.insert("FileName".into(), Value::String("bsky-app.webloc".into()));
        files_entry.insert("FileType".into(), Value::String("com.apple.web-internet-location".into()));
        files_entry.insert("FileSize".into(), Value::Integer(83.into()));
        files_entry.insert("FileBomPath".into(), Value::String("./bsky-app.webloc".into()));

        let mut root = Dictionary::new();
        root.insert("TransferID".into(), Value::String("A1B2C3D4-E5F6-7890-1234-567890ABCDEF".into()));
        root.insert("TransferType".into(), Value::String("links".into()));
        root.insert("SenderID".into(), Value::Data(vec![0xDE, 0xAD, 0xBE, 0xEF]));
        root.insert("SenderComputerName".into(), Value::String("iPhone42".into()));
        root.insert("SenderModelName".into(), Value::String("iPhone".into()));
        root.insert("BundleID".into(), Value::String("com.apple.mobilesafari".into()));
        root.insert("Files".into(), Value::Array(vec![Value::Dictionary(files_entry)]));
        root.insert("Items".into(), Value::Array(vec![]));

        let mut buf = Vec::new();
        Value::Dictionary(root).to_writer_binary(&mut buf).unwrap();
        buf
    }

    fn sample_config() -> ReceiverConfig {
        ReceiverConfig {
            computer_name: "luftlift-pc".into(),
            computer_model: "MacBookPro".into(),
            record_data: None,
        }
    }

    // ---- Discover response ----

    #[test]
    fn discover_response_is_valid_binary_plist_with_required_fields() {
        let cfg = sample_config();
        let body = build_discover_response(&cfg);
        assert!(!body.is_empty(), "response must not be empty");
        assert_eq!(&body[..8], b"bplist00", "must be a binary plist");

        let val = Value::from_reader(std::io::Cursor::new(&body[..])).expect("response parses as plist");
        let dict = val.as_dictionary().expect("root is a dict");
        assert_eq!(
            dict.get("ReceiverComputerName").and_then(|v| v.as_string()),
            Some("luftlift-pc"),
        );
        assert_eq!(
            dict.get("ReceiverModelName").and_then(|v| v.as_string()),
            Some("MacBookPro"),
        );
        // ReceiverMediaCapabilities is a JSON blob stored as raw data.
        let caps = dict.get("ReceiverMediaCapabilities").expect("caps present");
        let caps_bytes = caps.as_data().expect("caps is data");
        let caps_json = std::str::from_utf8(caps_bytes).unwrap();
        assert!(caps_json.contains("\"Version\""), "caps JSON has Version key");
    }

    #[test]
    fn discover_response_media_caps_matches_python_json_dumps_whitespace() {
        // Python json.dumps({"Version": 1}) -> '{"Version": 1}' (space after colon).
        // Verify byte-exactness against the reference shape.
        let cfg = sample_config();
        let body = build_discover_response(&cfg);
        let val = Value::from_reader(std::io::Cursor::new(&body[..])).unwrap();
        let caps_bytes = val
            .as_dictionary()
            .unwrap()
            .get("ReceiverMediaCapabilities")
            .unwrap()
            .as_data()
            .unwrap();
        assert_eq!(caps_bytes, b"{\"Version\": 1}");
    }

    // ---- Ask response ----

    #[test]
    fn ask_response_is_valid_binary_plist_with_required_fields() {
        let cfg = sample_config();
        let body = build_ask_response(&cfg);
        assert!(!body.is_empty());
        assert_eq!(&body[..8], b"bplist00");

        let val = Value::from_reader(std::io::Cursor::new(&body[..])).expect("response parses");
        let dict = val.as_dictionary().expect("root is dict");
        assert_eq!(
            dict.get("ReceiverComputerName").and_then(|v| v.as_string()),
            Some("luftlift-pc"),
        );
        assert_eq!(
            dict.get("ReceiverModelName").and_then(|v| v.as_string()),
            Some("MacBookPro"),
        );
    }

    // ---- Ask request parse ----

    #[test]
    fn parse_ask_request_extracts_fields_from_captured_shape() {
        let body = make_ask_request_plist();
        let req = parse_ask_request(&body).unwrap();
        assert_eq!(req.transfer_id, "A1B2C3D4-E5F6-7890-1234-567890ABCDEF");
        assert_eq!(req.transfer_type, "links");
        assert_eq!(req.sender_computer_name, "iPhone42");
        assert_eq!(req.sender_model_name, "iPhone");
        assert_eq!(req.bundle_id, "com.apple.mobilesafari");
        assert_eq!(req.files.len(), 1);
        assert_eq!(req.files[0].file_name, "bsky-app.webloc");
        assert_eq!(req.files[0].file_type, "com.apple.web-internet-location");
        assert_eq!(req.files[0].file_size, 83);
        assert_eq!(req.files[0].file_bom_path, "./bsky-app.webloc");
    }

    #[test]
    fn parse_ask_request_handles_multiple_files() {
        let mut root = Dictionary::new();
        root.insert("TransferID".into(), Value::String("T1".into()));
        root.insert("TransferType".into(), Value::String("files".into()));
        root.insert("SenderComputerName".into(), Value::String("Mac".into()));
        root.insert("SenderModelName".into(), Value::String("MacBook".into()));
        root.insert("BundleID".into(), Value::String("com.apple.finder".into()));
        let mut files = Vec::new();
        for (name, sz) in [("a.jpg", 100), ("b.png", 200), ("c.txt", 3)] {
            let mut f = Dictionary::new();
            f.insert("FileName".into(), Value::String(name.into()));
            f.insert("FileType".into(), Value::String("public.jpeg".into()));
            f.insert("FileSize".into(), Value::Integer(sz.into()));
            f.insert("FileBomPath".into(), Value::String(format!("./{name}")));
            files.push(Value::Dictionary(f));
        }
        root.insert("Files".into(), Value::Array(files));

        let mut buf = Vec::new();
        Value::Dictionary(root).to_writer_binary(&mut buf).unwrap();

        let req = parse_ask_request(&buf).unwrap();
        assert_eq!(req.files.len(), 3);
        assert_eq!(req.files[2].file_name, "c.txt");
        assert_eq!(req.files[2].file_size, 3);
    }
}
