//! The client address behind our own proxies, and the rate-limit keys derived from it.

use std::net::{IpAddr, Ipv6Addr};

use axum::http::HeaderMap;

use crate::config::AppConfig;
use crate::security::hash_opaque_token;

/// Normalizes a candidate address: IPv4 stays as is (a port is dropped), IPv6 is reduced to its /64 prefix
/// (one IPv6 host owns a whole /64), IPv4-mapped IPv6 becomes IPv4. `None` for anything that is not an IP.
fn normalize_address(raw: &str) -> Option<String> {
    let mut value = raw.trim();
    if let Some(inner) = value.strip_prefix('[') {
        // `[addr]` or `[addr]:port`
        let (addr, rest) = inner.split_once(']')?;
        if !(rest.is_empty()
            || (rest.starts_with(':')
                && rest[1..].bytes().all(|b| b.is_ascii_digit())
                && rest.len() > 1))
        {
            return None;
        }
        value = addr;
    } else if let Some((host, port)) = value.rsplit_once(':') {
        // `1.2.3.4:80` (a bare IPv6 has several colons and is left alone)
        if !host.contains(':') && !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) {
            value = host;
        }
    }
    let value = value.split('%').next().unwrap_or("");
    match value.parse::<IpAddr>().ok()? {
        IpAddr::V4(v4) => Some(v4.to_string()),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => Some(v4.to_string()),
            None => Some(prefix64(v6)),
        },
    }
}

fn prefix64(address: Ipv6Addr) -> String {
    let segments = address.segments();
    format!(
        "{:x}:{:x}:{:x}:{:x}::/64",
        segments[0], segments[1], segments[2], segments[3]
    )
}

/// Client address from the configured header. `x-forwarded-for` is read from the right: with
/// `trust_proxy_hops = n` the n-th entry from the end was added by our own proxy, and entries to its left are
/// set by the client and never trusted. Single-value headers are taken as is.
pub fn client_address(headers: &HeaderMap, config: &AppConfig) -> Option<String> {
    let value = headers
        .get(config.client_ip_header.as_str())?
        .to_str()
        .ok()?;
    if config.client_ip_header != "x-forwarded-for" {
        return normalize_address(value);
    }
    let hops = config.trust_proxy_hops?;
    let entries: Vec<&str> = value.split(',').collect();
    let entry = entries
        .len()
        .checked_sub(hops)
        .map(|index| entries[index])?;
    normalize_address(entry)
}

/// The client address cannot be determined in production (it would fail closed rather than share one counter).
#[derive(Debug, PartialEq, Eq)]
pub struct AddressUnavailable;

/// Rate-limit key `<scope>:ip:<sha256(address)>`: no personal data and a fixed length. An unknown address fails
/// closed in production instead of sharing one counter between all clients; elsewhere it falls back to
/// `<scope>:ip:local`.
pub fn client_key(
    headers: &HeaderMap,
    scope: &str,
    config: &AppConfig,
) -> Result<String, AddressUnavailable> {
    match client_address(headers, config) {
        Some(address) => Ok(format!("{scope}:ip:{}", hash_opaque_token(&address))),
        None if config.production => Err(AddressUnavailable),
        None => Ok(format!("{scope}:ip:local")),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn config(header: &str, hops: Option<usize>, production: bool) -> AppConfig {
        let mut config = AppConfig::from_lookup(|_| None).unwrap();
        config.client_ip_header = header.into();
        config.trust_proxy_hops = hops;
        config.production = production;
        config
    }

    fn headers(name: &'static str, value: &str) -> HeaderMap {
        let mut map = HeaderMap::new();
        map.insert(name, HeaderValue::from_str(value).unwrap());
        map
    }

    #[test]
    fn normalizes_addresses() {
        for (raw, expected) in [
            ("203.0.113.7", Some("203.0.113.7")),
            (" 203.0.113.7:5555 ", Some("203.0.113.7")),
            ("::ffff:203.0.113.7", Some("203.0.113.7")),
            ("2001:db8:1:2:3:4:5:6", Some("2001:db8:1:2::/64")),
            ("[2001:db8:1:2:3:4:5:6]:443", Some("2001:db8:1:2::/64")),
            ("[2001:db8::1]", Some("2001:db8:0:0::/64")),
            ("fe80::1%eth0", Some("fe80:0:0:0::/64")),
            ("::1", Some("0:0:0:0::/64")),
            ("not an ip", None),
            ("", None),
            ("[2001:db8::1]x", None),
            ("[2001:db8::1]:", None),
            ("[2001:db8::1", None),
            ("1.2.3.4:port", None),
        ] {
            assert_eq!(normalize_address(raw).as_deref(), expected, "{raw:?}");
        }
    }

    #[test]
    fn forwarded_for_is_read_from_the_right() {
        let cfg = config("x-forwarded-for", Some(1), false);
        let h = headers("x-forwarded-for", "6.6.6.6, 198.51.100.1, 203.0.113.9");
        assert_eq!(client_address(&h, &cfg).as_deref(), Some("203.0.113.9"));
        let cfg2 = config("x-forwarded-for", Some(2), false);
        assert_eq!(client_address(&h, &cfg2).as_deref(), Some("198.51.100.1"));
        // more hops than entries: nothing is trusted
        assert_eq!(
            client_address(&h, &config("x-forwarded-for", Some(4), false)),
            None
        );
    }

    #[test]
    fn forwarded_for_without_trusted_hops_is_ignored() {
        let h = headers("x-forwarded-for", "203.0.113.9");
        assert_eq!(
            client_address(&h, &config("x-forwarded-for", None, false)),
            None
        );
    }

    #[test]
    fn single_value_headers_are_taken_as_is() {
        let cfg = config("x-real-ip", None, false);
        assert_eq!(
            client_address(&headers("x-real-ip", "203.0.113.9"), &cfg).as_deref(),
            Some("203.0.113.9")
        );
        assert_eq!(client_address(&HeaderMap::new(), &cfg), None);
        let mut bad = HeaderMap::new();
        bad.insert("x-real-ip", HeaderValue::from_bytes(&[0xff]).unwrap());
        assert_eq!(client_address(&bad, &cfg), None);
    }

    #[test]
    fn keys_hash_the_address_and_fail_closed_only_in_production() {
        let cfg = config("x-real-ip", None, false);
        let key = client_key(&headers("x-real-ip", "203.0.113.9"), "login", &cfg).unwrap();
        assert_eq!(
            key,
            format!("login:ip:{}", hash_opaque_token("203.0.113.9"))
        );
        assert_eq!(
            client_key(&HeaderMap::new(), "login", &cfg).unwrap(),
            "login:ip:local"
        );
        let prod = config("x-real-ip", None, true);
        assert_eq!(
            client_key(&HeaderMap::new(), "login", &prod),
            Err(AddressUnavailable)
        );
    }
}
