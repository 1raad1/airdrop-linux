//! luftlift-rs — Rust port of opendrop (the AirDrop application layer).
//!
//! Runs over the `awdl0` interface that `filin-rs` brings up. See PORT.md for the
//! full spec and the reverse-engineered iOS-18 protocol details (dvzip, chunked
//! Ask/Discover, etc.).
//!
//! Reference: ../../opendrop-patch/ (patched Python opendrop + dvzip.py decoder).

pub mod chunked;
pub mod client;
pub mod cpio;
pub mod dvzip;
pub mod handlers;
pub mod http;
pub mod introspect;
pub mod mdns;
pub mod netutil;
pub mod plist_impl;
pub mod server;
pub mod tls;
