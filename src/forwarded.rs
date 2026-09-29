//! `X-Forwarded-*` rules and IP ranges. Pure functions.

use std::net::IpAddr;

use http::HeaderMap;

/// A header change: set a value, or remove the header (`None`).
pub type Change = (String, Option<String>);

/// Client identity headers that some servers trust. Without trust, the
/// proxy removes them. It also removes all other `x-forwarded-*` headers.
const IDENTITY_HEADERS: [&str; 7] = [
    "forwarded",
    "x-real-ip",
    "true-client-ip",
    "x-client-ip",
    "cf-connecting-ip",
    "x-cluster-client-ip",
    "x-original-forwarded-for",
];

/// The three headers that the proxy sets.
const SET: [&str; 3] = ["x-forwarded-for", "x-forwarded-proto", "x-forwarded-host"];

/// An IP address or a CIDR range, for example `10.0.0.0/8`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpRange {
    base: IpAddr,
    bits: u32,
}

impl IpRange {
    /// Parse `10.0.0.1`, `10.0.0.0/8`, `::1` or `fd00::/8`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let (base, bits) = match text.split_once('/') {
            Some((base, bits)) => (base, Some(bits.parse::<u32>().ok()?)),
            None => (text, None),
        };
        let base = base.parse::<IpAddr>().ok()?;
        let width = if base.is_ipv4() { 32 } else { 128 };
        let bits = bits.unwrap_or(width);
        (bits <= width).then_some(Self { base, bits })
    }

    /// `true` when `ip` is in the range.
    #[must_use]
    pub fn contains(&self, ip: IpAddr) -> bool {
        let (base, ip, width) = match (self.base, ip) {
            (IpAddr::V4(base), IpAddr::V4(ip)) => {
                (u128::from(u32::from(base)), u128::from(u32::from(ip)), 32)
            }
            (IpAddr::V6(base), IpAddr::V6(ip)) => (u128::from(base), u128::from(ip), 128),
            _ => return false,
        };
        let shift = width - self.bits;
        base.checked_shr(shift).unwrap_or(0) == ip.checked_shr(shift).unwrap_or(0)
    }
}

/// The header changes for the upstream request.
///
/// Without `trust`, the proxy replaces the client values and removes other
/// identity headers. A client must not choose the address that the
/// upstream sees.
#[must_use]
pub fn changes(
    client: Option<IpAddr>,
    incoming: &HeaderMap,
    host: Option<&str>,
    trust: bool,
) -> Vec<Change> {
    let kept = |name: &str| trust.then(|| joined(incoming, name)).flatten();
    let client = client.map(|ip| ip.to_string());
    let for_value = match (kept("x-forwarded-for"), client) {
        (Some(chain), Some(ip)) => Some(format!("{chain}, {ip}")),
        (chain, ip) => ip.or(chain),
    };
    let proto = kept("x-forwarded-proto").unwrap_or_else(|| "http".to_owned());
    let forwarded_host = kept("x-forwarded-host").or_else(|| host.map(str::to_owned));
    let mut out: Vec<Change> = SET
        .iter()
        .map(|name| (*name).to_owned())
        .zip([for_value, Some(proto), forwarded_host])
        .collect();
    if !trust {
        let others = incoming
            .keys()
            .map(http::HeaderName::as_str)
            .filter(|name| name.starts_with("x-forwarded-") && !SET.contains(name));
        let mut removed: Vec<String> = IDENTITY_HEADERS
            .iter()
            .copied()
            .chain(others)
            .map(str::to_owned)
            .collect();
        removed.dedup();
        out.extend(removed.into_iter().map(|name| (name, None)));
    }
    out
}

/// All values of `name`, joined with `, `. `None` when absent or not text.
fn joined(headers: &HeaderMap, name: &str) -> Option<String> {
    let values: Vec<&str> = headers
        .get_all(name)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect();
    (!values.is_empty()).then(|| values.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    /// `None`: no change. `Some(None)`: remove. `Some(Some(v))`: set `v`.
    #[allow(clippy::option_option)]
    fn get<'a>(changes: &'a [Change], name: &str) -> Option<Option<&'a str>> {
        changes
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_deref())
    }

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(*name, HeaderValue::from_static(value));
        }
        map
    }

    const CLIENT: Option<IpAddr> = Some(IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 9)));

    #[test]
    fn untrusted_client_values_are_replaced() {
        let incoming = headers(&[
            ("x-forwarded-for", "1.2.3.4"),
            ("x-forwarded-proto", "https"),
            ("x-forwarded-host", "evil.example"),
            ("forwarded", "for=1.2.3.4"),
        ]);
        let out = changes(CLIENT, &incoming, Some("shop.example"), false);
        assert_eq!(get(&out, "x-forwarded-for"), Some(Some("203.0.113.9")));
        assert_eq!(get(&out, "x-forwarded-proto"), Some(Some("http")));
        assert_eq!(get(&out, "x-forwarded-host"), Some(Some("shop.example")));
        assert_eq!(get(&out, "forwarded"), Some(None), "removed");
    }

    #[test]
    fn untrusted_identity_headers_are_removed() {
        let incoming = headers(&[
            ("x-real-ip", "10.0.0.1"),
            ("true-client-ip", "10.0.0.1"),
            ("x-forwarded-port", "443"),
            ("x-forwarded-prefix", "/admin"),
            ("x-forwarded-ssl", "on"),
        ]);
        let out = changes(CLIENT, &incoming, None, false);
        for name in [
            "x-real-ip",
            "true-client-ip",
            "x-forwarded-port",
            "x-forwarded-prefix",
            "x-forwarded-ssl",
            "cf-connecting-ip",
        ] {
            assert_eq!(get(&out, name), Some(None), "{name}");
        }
        let trusted = changes(CLIENT, &incoming, None, true);
        assert_eq!(get(&trusted, "x-real-ip"), None, "kept when trusted");
    }

    #[test]
    fn ranges_contain_addresses() {
        let lo: IpAddr = "127.0.0.1".parse().unwrap();
        for range in ["127.0.0.1", "127.0.0.0/8", "0.0.0.0/0", " 127.0.0.1/32 "] {
            assert!(IpRange::parse(range).unwrap().contains(lo), "{range}");
        }
        for range in ["10.0.0.0/8", "127.0.0.2", "::1"] {
            assert!(!IpRange::parse(range).unwrap().contains(lo), "{range}");
        }
        for bad in ["junk", "127.0.0.0/40", "::/129", "10.0.0.0/x", ""] {
            assert!(IpRange::parse(bad).is_none(), "{bad}");
        }
        let v6: IpAddr = "::1".parse().unwrap();
        assert!(IpRange::parse("::/0").unwrap().contains(v6));
        assert!(IpRange::parse("::1/128").unwrap().contains(v6));
        assert!(
            IpRange::parse("fd00::/8")
                .unwrap()
                .contains("fd12::1".parse().unwrap())
        );
    }

    #[test]
    fn trusted_client_values_are_kept_and_the_client_is_appended() {
        let incoming = headers(&[
            ("x-forwarded-for", "1.2.3.4"),
            ("x-forwarded-for", "5.6.7.8"),
            ("x-forwarded-proto", "https"),
            ("x-forwarded-host", "public.example"),
            ("forwarded", "for=1.2.3.4"),
        ]);
        let out = changes(CLIENT, &incoming, Some("internal"), true);
        assert_eq!(
            get(&out, "x-forwarded-for"),
            Some(Some("1.2.3.4, 5.6.7.8, 203.0.113.9"))
        );
        assert_eq!(get(&out, "x-forwarded-proto"), Some(Some("https")));
        assert_eq!(get(&out, "x-forwarded-host"), Some(Some("public.example")));
        assert_eq!(get(&out, "forwarded"), None, "untouched");
    }

    #[test]
    fn trusted_but_absent_values_are_set() {
        let out = changes(CLIENT, &HeaderMap::new(), Some("a.example"), true);
        assert_eq!(get(&out, "x-forwarded-for"), Some(Some("203.0.113.9")));
        assert_eq!(get(&out, "x-forwarded-proto"), Some(Some("http")));
        assert_eq!(get(&out, "x-forwarded-host"), Some(Some("a.example")));
    }

    #[test]
    fn no_client_and_no_host_remove_the_headers() {
        let incoming = headers(&[("x-forwarded-for", "1.2.3.4"), ("x-forwarded-host", "x")]);
        let out = changes(None, &incoming, None, false);
        assert_eq!(get(&out, "x-forwarded-for"), Some(None));
        assert_eq!(get(&out, "x-forwarded-host"), Some(None));
    }

    #[test]
    fn ipv6_clients_are_bare_addresses() {
        let client = Some("2001:db8::1".parse().unwrap());
        let out = changes(client, &HeaderMap::new(), None, false);
        assert_eq!(get(&out, "x-forwarded-for"), Some(Some("2001:db8::1")));
    }
}
