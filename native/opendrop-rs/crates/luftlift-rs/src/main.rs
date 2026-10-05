//! luftlift — Rust port of opendrop (the AirDrop application layer).
//!
//! Runs over the `awdl0` interface that `filin-rs` brings up. See PORT.md for
//! the full spec and the reverse-engineered iOS-18 protocol details (dvzip,
//! chunked Ask/Discover, etc.).
//!
//! Reference: ../../opendrop-patch/ (patched Python opendrop + dvzip.py decoder).

use anyhow::Result;
use clap::{Parser, Subcommand};
use luftlift_rs::{
    client::{AirDropTransport, FileToSend, RustlsTransport, SenderConfig, send_files, build_discover_request, parse_discover_response},
    introspect::{
        DEFAULT_HTTP_ADDR, IntrospectState, TraceCollector,
        spawn_http_server,
    },
    mdns, netutil, server, tls,
    mdns::DEFAULT_FLAGS,
    plist_impl::ReceiverConfig,
};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Parser)]
#[command(name = "luftlift", about = "AirDrop receiver (port of opendrop)")]
struct Cli {
    /// Network interface to bind (e.g. awdl0).
    #[arg(short, long, default_value = "awdl0")]
    interface: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Receive files via AirDrop (announce + HTTPS server).
    Receive {
        /// Directory to save received files (default: current dir).
        #[arg(short, long, default_value = ".")]
        output: PathBuf,
        /// AirDrop `flags` TXT value (default 0x06 = opendrop's verified value
        /// that makes us appear in iOS 18's AirDrop picker). Override for
        /// experimentation; e.g. `--flags 0x01`.
        #[arg(short = 'F', long, default_value_t = DEFAULT_FLAGS, value_parser = parse_flags)]
        flags: u32,
        /// Receiver display name shown in the Apple AirDrop picker
        /// (ReceiverComputerName in /Discover + /Ask responses). Defaults to
        /// `<base>-<pid>` so a fresh run is distinguishable from stale ghosts;
        /// the base comes from LUFTLIFT_NAME or "luftlift". An explicit
        /// `--name foo` is used verbatim (no PID suffix).
        #[arg(short = 'n', long)]
        name: Option<String>,
        /// Bind address for the loopback JSON introspection API
        /// (`/status`, `/trace`). Defaults to `127.0.0.1:9931`. Use
        /// `--http-addr off` to disable it entirely. SEPARATE from the
        /// AirDrop HTTPS listener on 8771; never reachable off-box.
        #[arg(long, default_value = DEFAULT_HTTP_ADDR)]
        http_addr: String,
    },
    /// Find AirDrop senders on the local network.
    Find {
        /// Browse duration in seconds.
        #[arg(short, long, default_value_t = 5)]
        duration: u64,
        /// Resolve display names and emit JSON for the desktop sender.
        #[arg(long)]
        json: bool,
    },
    /// Send a file to a specific AirDrop receiver.
    Send {
        /// Recipient identifier (mDNS instance name).
        recipient: String,
        /// File to send.
        #[arg(num_args = 1..)]
        file: Vec<PathBuf>,
        /// Previously discovered peer address, including its interface scope.
        #[arg(long)]
        address: Option<std::net::SocketAddr>,
    },
}

/// Parse a flags integer in decimal or hex (e.g. `6`, `0x06`).
fn parse_flags(s: &str) -> Result<u32, String> {
    let s = s.trim();
    let lower = s.to_ascii_lowercase();
    let radix_base = if lower.starts_with("0x") { 16 } else { 10 };
    let stripped = lower.strip_prefix("0x").unwrap_or(s);
    u32::from_str_radix(stripped, radix_base)
        .map_err(|e| format!("invalid flags value '{s}': {e}"))
}

fn receiver_port() -> u16 {
    std::env::var("LUFTLIFT_PORT").ok().and_then(|s| s.parse().ok()).unwrap_or(8771)
}

fn main() -> Result<()> {
    // Build the introspection state up-front so the TraceCollector layer can
    // be installed alongside the fmt layer from the start. The HTTP server
    // for the introspect API is only spawned by `receive`; `find` and `send`
    // never expose it (the layer is a no-op until set_trace_level is called,
    // which only POST /trace can do, and only `receive` serves that).
    let introspect = Arc::new(IntrospectState::new());

    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        // Default: our crate at debug, info elsewhere, but silence the
        // mdns-sd 0.11 NSEC parse spam ("Invalid incoming DNS message:
        // NSEC block length must be in the range 1-32: 0") triggered by
        // other devices' mDNS records on the LAN — not our bug, but it
        // drowns `luftlift find` output. RUST_LOG still overrides this.
        "luftlift_rs=debug,info,mdns_sd=off".into()
    });
    let registry = tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(TraceCollector::new(introspect.clone()));
    registry.init();

    let cli = Cli::parse();

    match cli.command {
        Command::Receive { output, flags, name, http_addr } => {
            receive(&cli.interface, output, flags, name, http_addr, introspect)
        }
        Command::Find { duration, json } => find(&cli.interface, duration, json),
        Command::Send { recipient, file, address } => send(&cli.interface, &recipient, file, address),
    }
}

fn receive(
    interface: &str,
    output: PathBuf,
    flags: u32,
    name: Option<String>,
    http_addr: String,
    introspect: Arc<IntrospectState>,
) -> Result<()> {
    tracing::info!(interface, flags = format!("0x{flags:02x}"), "starting AirDrop receiver");

    let cert = tls::generate_self_signed_cert()?;
    let tls_config = tls::build_server_config(&cert)?;
    tracing::info!("TLS cert generated");

    // The AirDrop picker displays ReceiverComputerName (from the /Discover and
    // /Ask response plists), NOT the mDNS instance name. Default to
    // `<base>-<pid>` so a fresh run is distinguishable from stale ghosts; an
    // explicit --name is used verbatim. The mDNS instance name continues to
    // embed the PID separately (in build_airdrop_service_info) for the same
    // reason at the mDNS layer.
    let base_name = default_base_name();
    let receiver_name = resolve_receiver_name(&name, &base_name);
    tracing::info!(receiver_name = %receiver_name, "receiver display name (shown in AirDrop picker)");

    let cfg = ReceiverConfig {
        computer_name: receiver_name.clone(),
        computer_model: "MacBookPro".into(),
        record_data: None,
    };

    // Spawn the loopback introspect HTTP server unless explicitly disabled.
    // Bound to 127.0.0.1 only — never reachable over the AWDL link. The
    // server runs in its own thread so it does not block the receive path.
    if !http_addr.eq_ignore_ascii_case("off") {
        match spawn_http_server(&http_addr, introspect.clone()) {
            Ok(server) => tracing::info!(
                addr = %server.local_addr,
                "introspect API serving /status and /trace (loopback)"
            ),
            Err(e) => tracing::warn!(
                error = %e,
                addr = %http_addr,
                "introspect HTTP server failed to bind — continuing without it",
            ),
        }
    } else {
        tracing::info!("introspect HTTP server disabled (--http-addr off)");
    }

    // RESILIENCE: the awdl0 tap is owned by filin and is destroyed/recreated
    // (with a new MAC → new IPv6) whenever filin restarts or the USB Wi-Fi card
    // is bounced. The HTTPS listener below is bound to [::]:8771 (dual-stack)
    // and survives that, but the mDNS announce is tied to awdl0's specific
    // address. A supervisor thread (re)announces whenever awdl0's address
    // appears or changes, checking once per second — so the receiver
    // automatically reappears in the AirDrop picker after a card bounce
    // instead of advertising a dead/old address.
    let goodbye: Arc<Mutex<Option<Box<dyn Fn() + Send>>>> = Arc::new(Mutex::new(None));
    {
        let interface = interface.to_string();
        let base_name = default_base_name();
        let receiver_name = receiver_name.clone();
        let introspect = introspect.clone();
        let goodbye = goodbye.clone();
        std::thread::spawn(move || {
            mdns_announce_supervisor(
                &interface,
                flags,
                &base_name,
                &receiver_name,
                introspect,
                goodbye,
            );
        });
    }

    // SIGINT/SIGTERM: send the current mDNS TTL=0 goodbye (the supervisor keeps
    // it up to date for whatever address is currently announced) so Apple
    // devices drop us from their picker immediately, then exit.
    {
        let goodbye = goodbye.clone();
        ctrlc::set_handler(move || {
            tracing::info!("signal received — sending mDNS goodbye");
            if let Ok(g) = goodbye.lock() {
                if let Some(f) = g.as_ref() {
                    f();
                }
            }
            std::process::exit(0);
        })?;
    }

    // Bind only to the requested interface, never Ethernet or Tailscale.
    let addr = netutil::get_ipv6_for_interface(interface)
        .ok_or_else(|| anyhow::anyhow!("{interface} has no IPv6 address yet"))?;
    let scoped = netutil::connect_addr_with_scope(
        std::net::IpAddr::V6(addr), receiver_port(), netutil::if_index_for(interface),
    );
    let listener = std::net::TcpListener::bind(scoped)?;
    tracing::info!(address = %scoped, "HTTPS listener bound to receiver interface");
    tracing::info!(output = %output.display(), "saving received files to output dir");

    server::serve_forever(listener, Arc::new(tls_config), cfg, Some(output), introspect);
    Ok(())
}

/// Keep an mDNS `_airdrop._tcp` announcement alive on the CURRENT awdl0
/// address, re-announcing whenever that address appears or changes. Runs
/// forever, polling once per second. Tearing down the previous announcement
/// (stop its re-announcer, TTL=0 goodbye, shut the daemon down) before making
/// a new one avoids advertising a stale/old address after a card bounce.
fn mdns_announce_supervisor(
    interface: &str,
    flags: u32,
    base_name: &str,
    receiver_name: &str,
    introspect: Arc<IntrospectState>,
    goodbye: Arc<Mutex<Option<Box<dyn Fn() + Send>>>>,
) {
    let mdns_cfg = mdns::MdnsConfig {
        computer_name: base_name.to_string(),
        port: receiver_port(),
        flags,
    };
    // Teardown for the currently-active announce, and the address it advertises.
    let mut teardown: Option<Box<dyn Fn()>> = None;
    let mut last_addr: Option<std::net::Ipv6Addr> = None;

    loop {
        match netutil::get_ipv6_for_interface(interface) {
            // New or changed awdl0 address → (re)announce.
            Some(addr) if Some(addr) != last_addr => {
                if let Some(td) = teardown.take() {
                    td();
                }
                match mdns::announce(interface, &mdns_cfg, addr.into()) {
                    Ok((daemon, info)) => {
                        let daemon = Arc::new(daemon);
                        let fullname = info.get_fullname().to_string();
                        let (stop, _handle) = mdns::spawn_reannouncer(daemon.clone(), info);
                        introspect.record_announce(receiver_name.to_string(), receiver_port());
                        tracing::info!(%addr, service = %fullname, "mDNS (re)announced on awdl0");

                        // Update the ctrl-c goodbye to point at this daemon.
                        if let Ok(mut g) = goodbye.lock() {
                            let d = daemon.clone();
                            let fname = fullname.clone();
                            let stopping = stop.clone();
                            *g = Some(Box::new(move || {
                                stopping.store(true, Ordering::Release);
                                let _ = mdns::goodbye(&d, &fname);
                                // Allow queued multicast goodbyes to reach an
                                // AWDL listen window before the radio is removed.
                                std::thread::sleep(Duration::from_millis(1200));
                            }));
                        }
                        // Teardown: stop re-announcer, goodbye, shut daemon down.
                        let d = daemon.clone();
                        let fname = fullname.clone();
                        teardown = Some(Box::new(move || {
                            stop.store(true, Ordering::Release);
                            let _ = mdns::goodbye(&d, &fname);
                            let _ = d.shutdown();
                        }));
                        last_addr = Some(addr);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, %addr, "mDNS announce failed; retrying in 1s");
                    }
                }
            }
            // awdl0 lost its address → tear down so we re-announce on return.
            None if last_addr.is_some() => {
                if let Some(td) = teardown.take() {
                    td();
                }
                last_addr = None;
                tracing::warn!(interface, "awdl0 address gone; will re-announce when it returns");
            }
            _ => {}
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn find(interface: &str, duration: u64, json: bool) -> Result<()> {
    tracing::info!(interface, duration, "browsing for _airdrop._tcp");
    let mut peers = mdns::browse(interface, Duration::from_secs(duration))?;
    if json {
        let local = netutil::get_ipv6_for_interface(interface).map(std::net::IpAddr::V6);
        let tls_config = Arc::new(tls::build_client_config()?);
        let sender = SenderConfig::new(hostname(), "Linux", "com.luftlift.app");
        let mut records = Vec::new();
        let mut seen = std::collections::HashSet::new();
        // OWL adds link-local neighbors for heard AWDL peers. Query their
        // AirDrop service directly when multicast discovery is lost on the
        // single-channel radio. Addresses come from the live interface.
        if interface == "awdl0" {
            if let Ok(output) = std::process::Command::new("ip").args(["-j", "-6", "neighbour", "show", "dev", interface]).output() {
                if let Ok(neighbors) = serde_json::from_slice::<Vec<serde_json::Value>>(&output.stdout) {
                    for neighbor in neighbors {
                        if let Some(addr) = neighbor["dst"].as_str().and_then(|a| a.parse::<std::net::IpAddr>().ok()) {
                            if matches!(addr, std::net::IpAddr::V6(v6) if v6.is_unicast_link_local())
                                && !peers.iter().any(|p| p.addresses.contains(&addr)) {
                                peers.push(mdns::DiscoveredPeer { name: format!("awdl-{addr}"), addresses: vec![addr], port: 8770 });
                            }
                        }
                    }
                }
            }
        }
        for peer in &peers {
            if !seen.insert(peer.name.clone()) || peer.addresses.iter().any(|a| Some(*a) == local) { continue; }
            if let Some(addr) = peer.addresses.iter().find(|a| a.is_ipv6()) {
                let address = netutil::connect_addr_with_scope(*addr, peer.port, netutil::if_index_for(interface));
                let mut transport = RustlsTransport::new(tls_config.clone(), address).with_timeout(Duration::from_secs(8));
                let name = transport.send(&build_discover_request(&sender))
                    .and_then(|response| parse_discover_response(&response))
                    .map(|record| record.computer_name);
                let name = match name {
                    Ok(name) => name,
                    Err(error) => {
                        tracing::warn!(peer = %peer.name, %error, "could not resolve AirDrop display name");
                        continue;
                    }
                };
                records.push(serde_json::json!({"id":peer.name, "name":name, "address":address.to_string()}));
            }
        }
        println!("{}", serde_json::to_string(&records)?);
        return Ok(());
    }
    if peers.is_empty() {
        println!("No AirDrop senders found.");
        println!("Tip: Apple devices announce only while their sharing pane is open.");
    } else {
        println!("AirDrop senders found:");
        for peer in &peers {
            let addrs = peer
                .addresses
                .iter()
                .map(|a| a.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            println!("  {} ({}:{})", peer.name, addrs, peer.port);
        }
    }
    Ok(())
}

/// Base name for mDNS instance/hostname and the receiver name. Respects the
/// `LUFTLIFT_NAME` environment variable; defaults to "luftlift".
fn default_base_name() -> String {
    std::env::var("LUFTLIFT_NAME").unwrap_or_else(|_| "luftlift".to_string())
}

/// Resolve the ReceiverComputerName (what the Apple AirDrop picker displays).
///
/// - `--name foo` → `"foo"` (user's explicit choice, no PID suffix).
/// - no `--name` → `"<base>-<pid>"` (default; PID lets the human tell a fresh
///   run from a stale ghost in the picker, since the picker shows
///   ReceiverComputerName, not the mDNS instance name).
fn resolve_receiver_name(cli_name: &Option<String>, base: &str) -> String {
    match cli_name {
        Some(n) => n.clone(),
        None => format!("{}-{}", base, std::process::id()),
    }
}

/// Best-effort name for the sender identity.
fn hostname() -> String {
    default_base_name()
}

/// Send a file to an AirDrop receiver. `recipient` is matched against mDNS
/// instance names (a prefix match is enough; the full names are long).
fn send(interface: &str, recipient: &str, paths: Vec<PathBuf>, address: Option<std::net::SocketAddr>) -> Result<()> {
    let mut files = Vec::new();
    for file in paths {
        let data = std::fs::read(&file)?;
        tracing::info!(file = %file.display(), bytes = data.len(), "loaded file to send");
        files.push(FileToSend {
            name: file.file_name().and_then(|n| n.to_str()).ok_or_else(|| anyhow::anyhow!("invalid filename"))?.to_string(),
            uti_type: uti_for(&file).to_string(), data,
        });
    }
    let peer_addr = if let Some(address) = address { address } else {

    // Resolve the recipient via mDNS (browse briefly, pick the matching peer).
    tracing::info!(recipient, "browsing for receiver");
    let peers = mdns::browse(interface, Duration::from_secs(5))?;
    // Match the recipient by name ONLY. Do NOT fall back to "the first peer
    // discovered" — that silently targets whatever turned up first (typically
    // our own co-located `luftlift receive` service), sending the file to the
    // wrong host. If nothing matches, fail loudly and list what WAS found.
    let peer = peers
        .iter()
        .find(|p| p.name.contains(recipient))
        .ok_or_else(|| {
            let found = if peers.is_empty() {
                "<none>".to_string()
            } else {
                peers
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            anyhow::anyhow!(
                "no AirDrop receiver matching '{recipient}' found; discovered: [{found}] \
                 (Apple devices announce only while their sharing pane is open)"
            )
        })?;
    let addr = peer
        .addresses
        .first()
        .ok_or_else(|| anyhow::anyhow!("receiver {recipient} has no resolved address"))?;
    let port = peer.port;
    tracing::info!(peer = %peer.name, %addr, port, "receiver found");

    // The peer's address is IPv6 link-local on awdl0 (fe80::/10). Connecting
    // to a link-local address REQUIRES a scope_id (the interface zone), else
    // TcpStream::connect fails with EINVAL (os error 22). mDNS discovery
    // returns the address but not the zone, so we attach the scope from the
    // operator's `-i <interface>`. See LUFTLIFT_SEND_BUG.md.
    let scope_id = netutil::if_index_for(interface);
    let needs_scope = matches!(addr, std::net::IpAddr::V6(v6) if v6.is_unicast_link_local());
    if needs_scope && scope_id.is_none() {
        tracing::warn!(
            interface,
            "link-local peer but interface index could not be resolved; \
             connect will likely fail with EINVAL (os error 22)"
        );
    } else if needs_scope {
        tracing::info!(interface, scope_id = scope_id, "attaching IPv6 scope_id for link-local peer");
    }
    netutil::connect_addr_with_scope(*addr, port, scope_id)
    };

    let tls_config = Arc::new(tls::build_client_config()?);
    let mut transport = RustlsTransport::new(tls_config, peer_addr);

    let sender = SenderConfig::new(hostname(), "Linux", "com.apple.finder");
    let transfer_id = format!("{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        rand_u32(), rand_u16(), rand_u16(), rand_u16(), rand_u64() & 0xFFFFFFFFFFFF);
    send_files(&mut transport, &sender, &files, &transfer_id)?;
    println!("Send complete.");
    Ok(())
}

/// Best-effort UTI type from a file extension.
fn uti_for(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("jpg") | Some("jpeg") => "public.jpeg",
        Some("png") => "public.png",
        Some("heic") => "public.heic",
        Some("gif") => "com.compuserve.gif",
        Some("mov") => "com.apple.quicktime-movie",
        Some("mp4") => "public.mpeg-4",
        Some("pdf") => "com.adobe.pdf",
        Some("txt") | Some("md") => "public.plain-text",
        Some("webloc") => "com.apple.web-internet-location",
        _ => "public.data",
    }
}

fn rand_u32() -> u32 {
    use std::time::SystemTime;
    let s = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    (s.subsec_nanos()) ^ (s.as_secs() as u32)
}
fn rand_u16() -> u16 {
    (rand_u32() & 0xFFFF) as u16
}
fn rand_u64() -> u64 {
    use std::time::SystemTime;
    let s = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    s.as_nanos() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_receiver_name_default_includes_pid() {
        // No --name: default receiver name is "<base>-<pid>" so the Apple
        // picker (which shows ReceiverComputerName, not the mDNS instance
        // name) distinguishes fresh runs from stale ghosts.
        let name = resolve_receiver_name(&None, "luftlift");
        assert_eq!(name, format!("luftlift-{}", std::process::id()));
    }

    #[test]
    fn resolve_receiver_name_explicit_override_uses_verbatim() {
        // --name foo: the explicit name is used as-is (no PID suffix).
        let name = resolve_receiver_name(&Some("my-mac".into()), "luftlift");
        assert_eq!(name, "my-mac");
    }

    #[test]
    fn resolve_receiver_name_empty_string_override_is_verbatim() {
        // An explicit empty --name "" is still an override (edge case).
        let name = resolve_receiver_name(&Some(String::new()), "luftlift");
        assert_eq!(name, "");
    }
}
