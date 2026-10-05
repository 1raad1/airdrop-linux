//! Live smoke: spawn the introspect HTTP server on 127.0.0.1:9931, seed
//! some state, and idle so the operator can `curl localhost:9931/status | jq`
//! to confirm the loopback API works end-to-end. Run with:
//!     cargo run -p luftlift-rs --example introspect_smoke --release
//! then from another shell:
//!     curl -s localhost:9931/status | jq
//!     curl -s -X POST localhost:9931/trace -d '{"level":"debug"}'
//!     ... emit something that produces a tracing event? — see the smoke's
//!         own log line; the example doesn't install the TraceCollector so
//!         /trace stays empty by design (the smoke is just for /status).
//! Ctrl+C to exit.

use luftlift_rs::introspect::{IntrospectState, spawn_http_server};
use std::sync::Arc;
use std::time::Duration;

fn main() {
    tracing_subscriber::fmt().init();
    let state = Arc::new(IntrospectState::new());
    state.record_announce("luftlift-smoke".into(), 8771);
    state.bump_discover();
    state.bump_ask(Some("iPhone-smoke"));
    let server = spawn_http_server("127.0.0.1:9931", state.clone()).expect("bind");
    println!("introspect smoke listening on http://{}", server.local_addr);
    println!("try: curl -s {} | jq", server.local_addr);
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}
