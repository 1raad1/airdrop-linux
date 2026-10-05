//! Network interface helpers — IPv6 address resolution for binding to awdl0.
//!
//! On Linux, reads `/proc/net/if_inet6` to find an interface's IPv6 address.
//! This is what opendrop's `AirDropUtil.get_ip_for_interface` does (via
//! `netifaces` on the Python side).

use std::net::{IpAddr, Ipv6Addr, SocketAddr, SocketAddrV6};

/// Find the first IPv6 address on the named interface (typically the
/// link-local `fe80::` address on awdl0).
pub fn get_ipv6_for_interface(name: &str) -> Option<Ipv6Addr> {
    let content = std::fs::read_to_string("/proc/net/if_inet6").ok()?;
    for line in content.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 6 && parts[5] == name {
            return parse_inet6_addr(parts[0]);
        }
    }
    None
}

/// Parse a `/proc/net/if_inet6` hex address (32 hex chars, no colons)
/// into an `Ipv6Addr`.
fn parse_inet6_addr(hex: &str) -> Option<Ipv6Addr> {
    if hex.len() != 32 {
        return None;
    }
    let mut bytes = [0u8; 16];
    for i in 0..16 {
        bytes[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(Ipv6Addr::from(bytes))
}

/// Resolve a network interface name (e.g. `awdl0`) to its numeric index via
/// `libc::if_nametoindex`. This index is the IPv6 `scope_id` required to
/// `connect()` to a link-local (`fe80::/10`) peer — without it the kernel
/// returns `EINVAL` (os error 22) because it cannot disambiguate which link
/// the address is reachable over. Returns `None` on unknown interface / error.
pub fn if_index_for(name: &str) -> Option<u32> {
    // SAFETY: `if_nametoindex` reads `name` as a NUL-terminated C string via
    // CString and returns a u32; no global state, no mutable pointers handed
    // out. The CString allocation guards the NUL-terminator / interior-NUL
    // rejection.
    let c_name = std::ffi::CString::new(name).ok()?;
    let idx = unsafe { libc::if_nametoindex(c_name.as_ptr()) };
    if idx == 0 {
        None
    } else {
        Some(idx)
    }
}

/// Build the [`SocketAddr`] to *connect* to a peer at `addr:port`, attaching
/// the IPv6 `scope_id` when the peer is link-local (`fe80::/10`).
///
/// mDNS discovery returns the peer's link-local address but not the zone
/// (which link it lives on). Connecting to a link-local address without a
/// scope_id fails with `EINVAL`; this helper stamps the scope_id derived from
/// the operator's `-i <interface>` (`if_index_for`) so `TcpStream::connect`
/// succeeds. For non-link-local addresses (global IPv6, IPv4) `scope_id` is
/// ignored and a plain `SocketAddr::new` is returned.
///
/// **Pure** — the `if_nametoindex` call is split out into [`if_index_for`] so
/// this function is unit-testable without touching the OS.
pub fn connect_addr_with_scope(
    addr: IpAddr,
    port: u16,
    scope_id: Option<u32>,
) -> SocketAddr {
    match addr {
        IpAddr::V6(v6) if v6.is_unicast_link_local() => {
            let scope = scope_id.unwrap_or(0);
            SocketAddr::V6(SocketAddrV6::new(v6, port, 0, scope))
        }
        _ => SocketAddr::new(addr, port),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_inet6_addr_lo() {
        // ::1 as it appears in /proc/net/if_inet6
        let addr = parse_inet6_addr("00000000000000000000000000000001").unwrap();
        assert_eq!(addr, Ipv6Addr::LOCALHOST);
    }

    #[test]
    fn parse_inet6_addr_fe80() {
        // fe80::200:ff:fe12:3456 as 32 hex chars (no colons).
        let addr = parse_inet6_addr("fe80000000000000020000fffe123456").unwrap();
        assert!(addr.is_unicast_link_local());
    }

    #[test]
    fn parse_inet6_addr_rejects_short_hex() {
        assert!(parse_inet6_addr("abc").is_none());
    }

    #[test]
    fn get_ipv6_for_loopback_if_present() {
        // `lo` always has ::1 on Linux.
        if let Some(addr) = get_ipv6_for_interface("lo") {
            assert_eq!(addr, Ipv6Addr::LOCALHOST);
        }
    }

    // ---- connect_addr_with_scope (the scope_id fix) ----

    fn fe80() -> Ipv6Addr {
        "fe80::be05:43ff:fe0d:50a1".parse().unwrap()
    }

    #[test]
    fn link_local_peer_gets_scope_id_attached() {
        // A fe80:: peer must carry the interface index as scope_id, else
        // TcpStream::connect returns EINVAL (os error 22).
        let sa = connect_addr_with_scope(IpAddr::V6(fe80()), 8771, Some(42));
        match sa {
            SocketAddr::V6(v6) => {
                assert_eq!(v6.scope_id(), 42, "scope_id must be the interface index");
                assert_eq!(v6.port(), 8771);
                assert_eq!(*v6.ip(), fe80());
            }
            other => panic!("expected V6 socket addr, got {other:?}"),
        }
    }

    #[test]
    fn link_local_peer_with_no_scope_falls_back_to_zero() {
        // Without a resolved index we still build a V6 socket addr; the
        // caller logs a warning, and connect() will fail loudly at the
        // kernel rather than silently here.
        let sa = connect_addr_with_scope(IpAddr::V6(fe80()), 8771, None);
        match sa {
            SocketAddr::V6(v6) => assert_eq!(v6.scope_id(), 0),
            other => panic!("expected V6, got {other:?}"),
        }
    }

    #[test]
    fn non_link_local_ipv6_ignores_scope_id() {
        // Global/global-scope IPv6 must NOT carry an interface zone — the
        // kernel routes via the table, not per-link.
        let global: Ipv6Addr = "2001:db8::1".parse().unwrap();
        let sa = connect_addr_with_scope(IpAddr::V6(global), 443, Some(99));
        match sa {
            SocketAddr::V6(v6) => assert_eq!(v6.scope_id(), 0, "global IPv6 has no scope"),
            other => panic!("expected V6, got {other:?}"),
        }
    }

    #[test]
    fn ipv4_peer_ignored_scope_id() {
        let sa = connect_addr_with_scope(IpAddr::V4("192.0.2.1".parse().unwrap()), 443, Some(99));
        assert!(sa.is_ipv4(), "IPv4 must not be coerced to IPv6");
        // No scope_id concept on IPv4 — just port+addr.
        assert_eq!(sa.port(), 443);
    }

    #[test]
    fn localhost_ipv6_ignored_scope_id() {
        // ::1 is not link-local; scope_id must stay 0.
        let sa = connect_addr_with_scope(IpAddr::V6(Ipv6Addr::LOCALHOST), 8771, Some(7));
        match sa {
            SocketAddr::V6(v6) => assert_eq!(v6.scope_id(), 0),
            other => panic!("expected V6, got {other:?}"),
        }
    }

    #[test]
    fn if_index_for_loopback_is_one_on_linux() {
        // `lo` is interface index 1 on every Linux. Guards the libc wrapper.
        if let Some(idx) = if_index_for("lo") {
            assert_eq!(idx, 1, "`lo` is always ifindex 1 on Linux");
        }
        // (On a sandbox without `lo` this just returns None; the test is a
        // no-op there rather than failing.)
    }

    #[test]
    fn if_index_for_unknown_interface_is_none() {
        assert!(if_index_for("definitely-not-an-interface-xyzzy").is_none());
    }
}
