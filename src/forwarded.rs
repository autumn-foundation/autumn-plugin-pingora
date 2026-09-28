//! `X-Forwarded-*` rules. Pure functions.

use std::net::IpAddr;

use http::HeaderMap;

/// A header change: set a value, or remove the header (`None`).
pub type Change = (&'static str, Option<String>);

/// The forwarded headers for the upstream request.
///
/// Without `trust`, the proxy replaces client values: a client must not
/// choose the address that the upstream sees.
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
    let mut out = vec![
        ("x-forwarded-for", for_value),
        ("x-forwarded-proto", Some(proto)),
        ("x-forwarded-host", forwarded_host),
    ];
    if !trust {
        out.push(("forwarded", None));
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

    fn get<'a>(changes: &'a [Change], name: &str) -> Option<Option<&'a str>> {
        changes
            .iter()
            .find(|(n, _)| *n == name)
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
