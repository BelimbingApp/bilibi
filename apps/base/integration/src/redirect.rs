//! Where a redirect may go, and when the next request must drop credentials.
//!
//! Callers keep using `send` and `stream`. Set `Outgoing::redirects` instead of
//! following with a private client or with reqwest's default policy, which
//! would forward `Authorization` to the next host. The recorded exchange stays
//! the original URL: `redaction::endpoint` strips the query, and this module
//! never writes the `Location` value into the row.
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Which HTTPS hosts a call may be sent to after a redirect, and how many
/// hops are allowed. Hosts are exact names, such as `api.github.com`.
#[derive(Clone, Copy, Debug)]
pub struct RedirectPolicy {
    pub max_hops: u8,
    pub allowed_hosts: &'static [&'static str],
}

pub enum Choice {
    /// Request `url` next. `strip_credentials` is set when the host changed.
    Follow {
        url: String,
        strip_credentials: bool,
    },
    /// Do not follow. The call fails as `redirect` and nothing is sent there.
    Reject,
}

/// `Authorization`, `Cookie` and the other credential headers. Compared
/// without case, so a redirect to another host cannot carry them.
pub fn credential_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == "authorization"
        || name == "cookie"
        || name == "proxy-authorization"
        || name == "x-api-key"
        || name.contains("token")
        || name.contains("secret")
}

/// Decide the next request from a 3xx `Location`. `hops_done` is how many
/// redirects have already been followed.
pub fn choose(current: &str, location: &str, policy: &RedirectPolicy, hops_done: u8) -> Choice {
    if hops_done >= policy.max_hops || location.is_empty() || location.len() > 4096 {
        return Choice::Reject;
    }
    let Ok(base) = url::Url::parse(current) else {
        return Choice::Reject;
    };
    let Ok(next) = base.join(location) else {
        return Choice::Reject;
    };
    if !allowed(&next, policy) {
        return Choice::Reject;
    }
    let strip = !same_host(base.host_str(), next.host_str());
    Choice::Follow {
        url: next.to_string(),
        strip_credentials: strip,
    }
}

fn allowed(url: &url::Url, policy: &RedirectPolicy) -> bool {
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    if !port_ok(url.port()) || !public_destination(host) {
        return false;
    }
    policy
        .allowed_hosts
        .iter()
        .any(|allowed| host.eq_ignore_ascii_case(allowed))
}

fn port_ok(port: Option<u16>) -> bool {
    matches!(port, None | Some(443))
}

fn same_host(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
        _ => false,
    }
}

/// Reject loopback, private, link-local and metadata-style names even when
/// a caller listed them. A redirect is not a way onto this machine.
fn public_destination(host: &str) -> bool {
    let host = host.trim_matches(['[', ']']);
    if host.eq_ignore_ascii_case("localhost")
        || host.eq_ignore_ascii_case("metadata.google.internal")
        || host.to_ascii_lowercase().ends_with(".localhost")
        || host.to_ascii_lowercase().ends_with(".local")
    {
        return false;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(address)) => public_v4(address),
        Ok(IpAddr::V6(address)) => public_v6(address),
        Err(_) => true,
    }
}

fn public_v4(address: Ipv4Addr) -> bool {
    !address.is_private()
        && !address.is_loopback()
        && !address.is_link_local()
        && !address.is_unspecified()
        && !address.is_multicast()
        && !address.is_broadcast()
        && !address.is_documentation()
        && !reserved_v4(address)
}

/// `240.0.0.0/4`, the block Rust calls reserved, except the broadcast address.
fn reserved_v4(address: Ipv4Addr) -> bool {
    let [first, _, _, _] = address.octets();
    first >= 240 && address != Ipv4Addr::BROADCAST
}

fn public_v6(address: Ipv6Addr) -> bool {
    if let Some(v4) = address.to_ipv4_mapped() {
        return public_v4(v4);
    }
    !address.is_loopback()
        && !address.is_unspecified()
        && !address.is_multicast()
        && !address.is_unicast_link_local()
        && !address.is_unique_local()
}
