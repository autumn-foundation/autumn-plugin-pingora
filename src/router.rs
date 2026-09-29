//! Route match and prefix strip. Pure functions.

use crate::config::{Route, normalize_prefix};

/// Finds the route for a request.
///
/// The host decides first, as in nginx: an exact host, then the longest
/// wildcard, then routes with no host. In that group, the longest path
/// prefix wins. When no prefix matches, the next group applies.
#[derive(Debug, Clone, Default)]
pub struct Router {
    entries: Vec<Entry>,
}

#[derive(Debug, Clone)]
struct Entry {
    index: usize,
    host: HostRule,
    /// Without a trailing `/`. The root prefix is empty.
    prefix: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HostRule {
    Any,
    /// `.example.com` for `*.example.com`.
    Wildcard(String),
    Exact(String),
}

impl HostRule {
    fn parse(pattern: &str) -> Self {
        let pattern = pattern.trim().to_ascii_lowercase();
        if pattern.is_empty() {
            Self::Any
        } else if let Some(suffix) = pattern.strip_prefix('*') {
            Self::Wildcard(suffix.to_owned())
        } else {
            Self::Exact(pattern)
        }
    }

    /// Higher is more specific.
    const fn rank(&self) -> (u8, usize) {
        match self {
            Self::Any => (0, 0),
            Self::Wildcard(suffix) => (1, suffix.len()),
            Self::Exact(_) => (2, 0),
        }
    }

    fn matches(&self, host: Option<&str>) -> bool {
        match (self, host) {
            (Self::Any, _) => true,
            (_, None) => false,
            (Self::Exact(want), Some(host)) => want == host,
            (Self::Wildcard(suffix), Some(host)) => {
                host.len() > suffix.len() && host.ends_with(suffix.as_str())
            }
        }
    }
}

impl Router {
    /// A router over `routes`. The index of a match is the index in
    /// `routes`.
    #[must_use]
    pub fn new(routes: &[Route]) -> Self {
        let mut entries: Vec<Entry> = routes
            .iter()
            .enumerate()
            .map(|(index, route)| Entry {
                index,
                host: HostRule::parse(&route.host),
                prefix: normalize_prefix(&route.path_prefix),
            })
            .collect();
        entries.sort_by(|a, b| {
            b.host
                .rank()
                .cmp(&a.host.rank())
                .then_with(|| b.prefix.len().cmp(&a.prefix.len()))
                .then_with(|| a.index.cmp(&b.index))
        });
        Self { entries }
    }

    /// The index of the best route for `host` (a `Host` header value) and
    /// `path` (no query).
    #[must_use]
    pub fn find(&self, host: Option<&str>, path: &str) -> Option<usize> {
        let host = host.map(normalize_host);
        self.entries
            .iter()
            .find(|entry| {
                entry.host.matches(host.as_deref()) && prefix_matches(&entry.prefix, path)
            })
            .map(|entry| entry.index)
    }
}

/// Lower case, no port, no trailing `.`. `[::1]:80` gives `[::1]`.
fn normalize_host(raw: &str) -> String {
    let raw = raw.trim();
    let host = if raw.starts_with('[') {
        raw.find(']').map_or(raw, |end| &raw[..=end])
    } else {
        raw.split_once(':').map_or(raw, |(host, _)| host)
    };
    host.trim_end_matches('.').to_ascii_lowercase()
}

/// `prefix` (normalized) is a segment prefix of `path`.
fn prefix_matches(prefix: &str, path: &str) -> bool {
    if prefix.is_empty() {
        return path.starts_with('/');
    }
    path.strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// `true` when `path` can escape a route prefix on some upstream server.
///
/// The check decodes percent escapes two times (`%252e` becomes `.`). It
/// splits on `/` and removes `;params`. A path is unsafe when a
/// segment is `.` or `..`, or when it has a backslash or a NUL.
#[must_use]
pub fn unsafe_path(path: &str) -> bool {
    let path = path.split('?').next().unwrap_or(path);
    let decoded = percent_decode(&percent_decode(path));
    if decoded.contains(['\\', '\0']) || path.contains('\\') {
        return true;
    }
    decoded.split('/').any(|segment| {
        let segment = segment.split(';').next().unwrap_or(segment);
        segment == "." || segment == ".."
    })
}

/// Decode `%XX` escapes. Bad escapes stay as they are.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let escape = bytes
            .get(index + 1..index + 3)
            .filter(|_| bytes[index] == b'%')
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        if let Some(byte) = escape {
            out.push(byte);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The request target after the route prefix is removed. The query stays.
#[must_use]
pub fn strip_prefix(path_and_query: &str, prefix: &str) -> String {
    let prefix = normalize_prefix(prefix);
    let (path, query) = match path_and_query.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (path_and_query, None),
    };
    let Some(rest) = path.strip_prefix(prefix.as_str()) else {
        return path_and_query.to_owned();
    };
    let rest = if rest.is_empty() { "/" } else { rest };
    query.map_or_else(|| rest.to_owned(), |query| format!("{rest}?{query}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn route(name: &str, host: &str, prefix: &str) -> Route {
        let mut route = Route::default();
        name.clone_into(&mut route.name);
        host.clone_into(&mut route.host);
        prefix.clone_into(&mut route.path_prefix);
        route.upstreams = vec!["127.0.0.1:1".to_owned()];
        route
    }

    /// Routes and their router. `pick` gives the route name, or `-`.
    struct Table(Vec<Route>, Router);

    impl Table {
        fn new(list: Vec<Route>) -> Self {
            let router = Router::new(&list);
            Self(list, router)
        }

        fn pick(&self, host: Option<&str>, path: &str) -> &str {
            self.1
                .find(host, path)
                .map_or("-", |i| self.0[i].name.as_str())
        }
    }

    #[test]
    fn prefixes_match_on_a_segment_boundary() {
        let table = Table::new(vec![route("api", "", "/api")]);
        for path in ["/api", "/api/", "/api/users", "/api/a/b"] {
            assert_eq!(table.pick(None, path), "api", "{path}");
        }
        for path in ["/", "/apix", "/ap", "/API", ""] {
            assert_eq!(table.pick(None, path), "-", "{path}");
        }
    }

    #[test]
    fn a_trailing_slash_in_the_prefix_is_the_same_prefix() {
        let table = Table::new(vec![route("api", "", "/api/")]);
        assert_eq!(table.pick(None, "/api"), "api");
        assert_eq!(table.pick(None, "/api/x"), "api");
        assert_eq!(table.pick(None, "/apix"), "-");
    }

    #[test]
    fn the_longest_prefix_wins() {
        let table = Table::new(vec![
            route("root", "", "/"),
            route("api", "", "/api"),
            route("v2", "", "/api/v2"),
        ]);
        assert_eq!(table.pick(None, "/"), "root");
        assert_eq!(table.pick(None, "/other"), "root");
        assert_eq!(table.pick(None, "/api/v1"), "api");
        assert_eq!(table.pick(None, "/api/v2/x"), "v2");
        assert_eq!(table.pick(None, "/api/v20"), "api");
    }

    #[test]
    fn a_host_route_wins_over_an_any_host_route() {
        let table = Table::new(vec![
            route("any", "", "/api"),
            route("exact", "api.example.com", "/api"),
            route("wild", "*.example.com", "/api"),
        ]);
        assert_eq!(table.pick(Some("api.example.com"), "/api"), "exact");
        assert_eq!(table.pick(Some("API.Example.COM:8080"), "/api"), "exact");
        assert_eq!(table.pick(Some("api.example.com."), "/api"), "exact");
        assert_eq!(table.pick(Some("x.example.com"), "/api"), "wild");
        assert_eq!(table.pick(Some("a.b.example.com"), "/api"), "wild");
        assert_eq!(table.pick(Some("example.com"), "/api"), "any");
        assert_eq!(table.pick(Some("other.org"), "/api"), "any");
        assert_eq!(table.pick(None, "/api"), "any");
    }

    #[test]
    fn a_host_route_wins_over_a_longer_any_host_prefix() {
        let table = Table::new(vec![
            route("host", "a.com", "/"),
            route("host-api", "a.com", "/api"),
            route("deep", "", "/x/y"),
        ]);
        assert_eq!(table.pick(Some("a.com"), "/x/y/z"), "host");
        assert_eq!(table.pick(Some("a.com"), "/api/1"), "host-api");
        assert_eq!(table.pick(Some("b.com"), "/x/y/z"), "deep");
    }

    #[test]
    fn a_host_route_falls_through_when_no_prefix_matches() {
        let table = Table::new(vec![route("host", "a.com", "/v1"), route("any", "", "/")]);
        assert_eq!(table.pick(Some("a.com"), "/v1/x"), "host");
        assert_eq!(table.pick(Some("a.com"), "/other"), "any");
    }

    #[test]
    fn a_host_route_does_not_match_other_hosts() {
        let table = Table::new(vec![route("wild", "*.example.com", "/")]);
        assert_eq!(table.pick(Some("example.com"), "/"), "-");
        assert_eq!(table.pick(Some("badexample.com"), "/"), "-");
        assert_eq!(table.pick(None, "/"), "-");
        assert_eq!(table.pick(Some("[::1]:80"), "/"), "-");
    }

    #[test]
    fn ipv6_hosts_lose_the_port_only() {
        let table = Table::new(vec![route("v6", "[::1]", "/")]);
        assert_eq!(table.pick(Some("[::1]:8080"), "/"), "v6");
        assert_eq!(table.pick(Some("[::1]"), "/"), "v6");
    }

    #[test]
    fn strip_keeps_the_query_and_a_leading_slash() {
        for (target, prefix, want) in [
            ("/api/users?x=1", "/api", "/users?x=1"),
            ("/api/users", "/api/", "/users"),
            ("/api", "/api", "/"),
            ("/api?x=1", "/api", "/?x=1"),
            ("/api/", "/api", "/"),
            ("/other", "/", "/other"),
            ("/", "/", "/"),
        ] {
            assert_eq!(strip_prefix(target, prefix), want, "{target} - {prefix}");
        }
    }

    #[test]
    fn unsafe_paths_are_found() {
        for path in [
            "/a/../b",
            "/..",
            "/.",
            "/a/./b",
            "/a/%2e%2e/b",
            "/a/%2E./b",
            "/a/.%2e",
            "/a/..",
            "/a/..;/b",
            "/a/..;x=1/b",
            "/a/..%2fb",
            "/a/%2e%2e%2fb",
            "/a/%252e%252e/b",
            "/a/..\\b",
            "/a/%5c..",
            "/a/%00",
        ] {
            assert!(unsafe_path(path), "{path}");
        }
        for path in [
            "/",
            "/a/b",
            "/a..b",
            "/.well-known/x",
            "/a/...",
            "/a/b.c",
            "/files/a%2Fb",
            "/a;v=1/b",
            "/100%",
            "/a?x=/../",
        ] {
            assert!(!unsafe_path(path), "{path}");
        }
    }

    proptest! {
        /// A match always has a prefix that is a segment prefix of the path.
        #[test]
        fn a_match_is_a_segment_prefix(
            prefixes in proptest::collection::vec("(/[a-c]{1,2}){0,3}", 1..6),
            path in "(/[a-c]{0,2}){0,4}",
        ) {
            let routes: Vec<Route> = prefixes
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let prefix = if p.is_empty() { "/".to_owned() } else { p.clone() };
                    route(&format!("r{i}"), "", &prefix)
                })
                .collect();
            let router = Router::new(&routes);
            let path = if path.is_empty() { "/".to_owned() } else { path };
            let segment_prefix = |prefix: &str| {
                let with_slash = format!("{prefix}/");
                prefix.is_empty() || path == prefix || path.starts_with(&with_slash)
            };
            if let Some(i) = router.find(None, &path) {
                let prefix = routes[i].path_prefix.trim_end_matches('/');
                prop_assert!(segment_prefix(prefix));
                // No other match is longer.
                for other in &routes {
                    let p = other.path_prefix.trim_end_matches('/');
                    prop_assert!(!segment_prefix(p) || p.len() <= prefix.len());
                }
            } else {
                prop_assert!(routes.iter().all(|r| !segment_prefix(r.path_prefix.trim_end_matches('/'))));
            }
        }

        /// Strip gives a path that starts with `/` and ends with the rest.
        #[test]
        fn strip_gives_a_rooted_target(prefix in "(/[a-z]{1,3}){0,2}", rest in "(/[a-z]{0,3}){0,3}", query in "(\\?[a-z=]{0,4})?") {
            let prefix = if prefix.is_empty() { "/".to_owned() } else { prefix };
            let target = format!("{}{rest}{query}", prefix.trim_end_matches('/'));
            let target = if target.starts_with('/') { target } else { format!("/{target}") };
            let stripped = strip_prefix(&target, &prefix);
            let tail = format!("{rest}{query}");
            prop_assert!(stripped.starts_with('/'));
            prop_assert!(stripped.ends_with(&tail));
        }
    }
}
