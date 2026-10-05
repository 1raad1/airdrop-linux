//! filin-rs — Rust port of owl (Apple Wireless Direct Link daemon).
//!
//! See PORT.md in this crate for the full implementation spec and the
//! hard-won lessons (hardware-TSF sync, monitor mode, channel 44, etc.).
//!
//! Reference: ../../owl/  (C source; note our hardware-TSF patch in owl/src/rx.c)

fn main() -> anyhow::Result<()> {
    use tracing_subscriber::prelude::*;

    // Build the introspection handle first so both the fmt layer and the
    // ring-buffer TraceLayer can be installed before any tracing! call. The
    // fmt layer keeps its own EnvFilter; the TraceLayer is unfiltered (sees
    // every event) and gates capture internally via its AtomicU8 — so the
    // `/trace` endpoint can surface debug/trace events even when RUST_LOG
    // is set to info+ on the console.
    let introspect = filin_rs::introspect::Introspection::new();
    let fmt_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "filin_rs=debug,info".into());
    let registry = tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_filter(fmt_filter))
        .with(introspect.trace_layer());
    tracing::subscriber::set_global_default(registry)
        .map_err(|err| anyhow::anyhow!("failed to install tracing subscriber: {err:?}"))?;

    tracing::info!("filin starting");
    let (config, check) = filin_rs::cli::parse_config_from_env()
        .map_err(|err| anyhow::anyhow!("invalid CLI arguments: {err:?}"))?;

    // `--check`: preflight the adapter and exit. No link is brought up and no
    // root is required — this just asks the driver what it supports so an
    // operator can tell whether a given card is usable before committing.
    if check {
        let report = filin_rs::runtime::probe_adapter(&config.monitor_iface);
        println!("{}", report.summary());
        std::process::exit(if report.is_unusable() { 1 } else { 0 });
    }
    tracing::info!(
        monitor_iface = %config.monitor_iface,
        host_iface = %config.host_iface,
        anchor_channel = config.anchor_channel,
        assume_monitor = config.assume_monitor,
        pcap_path = ?config.pcap_path,
        http_addr = ?config.http_addr,
        "validated filin configuration"
    );
    // Resilience: the USB Wi-Fi card can be bounced (USB reset / replug /
    // firmware -110), which takes the monitor interface and our AF_PACKET
    // socket down. Rather than exit, retry open_links + run forever with a ~1s
    // backoff so filin recovers automatically when the card returns. `run`
    // already returns Err within ~1s once the monitor socket is stuck in
    // POLLERR (see POLL_ERROR_FATAL_THRESHOLD), so this loop reacts promptly.
    //
    // The introspection HTTP server is a detached thread spawned inside `run`;
    // we pass the bind address only on the FIRST run() so retries never try to
    // re-bind the (still-listening) port. `http_addr.take()` yields Some once,
    // then None — and only when open_links succeeds, so the server spawns on
    // the first run that actually starts, not the first attempt.
    let mut http_addr = config.http_addr;
    loop {
        match filin_rs::runtime::open_links(&config) {
            Ok(links) => {
                tracing::info!("filin links are up");
                match filin_rs::runtime::run(
                    links,
                    introspect.clone(),
                    http_addr.take(),
                    config.park,
                    config.disable_rssi_filter,
                    config.force_master,
                    config.tx_retransmits,
                ) {
                    Ok(()) => {
                        tracing::info!("filin runtime exited cleanly");
                        return Ok(());
                    }
                    Err(err) => tracing::warn!(
                        ?err,
                        "filin runtime stopped (monitor iface gone?); reopening links in 1s"
                    ),
                }
            }
            // A permanently-unusable adapter (e.g. no monitor mode) will never
            // work, so retrying every second is pointless noise — exit with the
            // operator-facing explanation already logged by open_links.
            Err(filin_rs::runtime::Error::UnsupportedAdapter(msg)) => {
                return Err(anyhow::anyhow!("unusable monitor adapter: {msg}"));
            }
            Err(err) => tracing::warn!(
                ?err,
                iface = %config.monitor_iface,
                "failed to open filin links (card not present?); retrying in 1s"
            ),
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
