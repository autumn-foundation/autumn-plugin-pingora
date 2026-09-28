//! Route match and prefix strip. Pure functions.

use crate::config::{RouteConfig, normalize_prefix};

/// Finds the route for a request.
///
/// The longest path prefix wins. At the same prefix, an exact host wins
/// over a wildcard host, and a host wins over no host.
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
    fn rank(&self) -> (u8, usize) {
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
    pub fn new(routes: &[RouteConfig]) -> Self {
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
            b.prefix
                .len()
                .cmp(&a.prefix.len())
                .then_with(|| b.host.rank().cmp(&a.host.rank()))
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

/// `true` when `path` has a `.` or `..` segment, also percent-encoded.
#[must_use]
pub fn has_dot_segment(path: &str) -> bool {
    let path = path.split('?').next().unwrap_or(path);
    path.split('/').any(|segment| {
        let decoded = segment.replace("%2e", ".").replace("%2E", ".");
        decoded == "." || decoded == ".."
    })
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
    match query {
        Some(query) => format!("{rest}?{query}"),
        None => rest.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn route(name: &str, host: &str, prefix: &str) -> RouteConfig {
        let mut route = RouteConfig::default();
        name.clone_into(&mut route.name);
        host.clone_into(&mut route.host);
        prefix.clone_into(&mut route.path_prefix);
        route.upstreams = vec!["127.0.0.1:1".to_owned()];
        route
    }

    fn names(routes: &[RouteConfig], router: &Router, host: Option<&str>, path: &str) -> String {
        router
            .find(host, path)
            .map_or_else(|| "-".to_owned(), |i| routes[i].name.clone())
    }

    #[test]
    fn prefixes_match_on_a_segment_boundary() {
        let routes = vec![route("api", "", "/api")];
        let router = Router::new(&routes);
        for path in ["/api", "/api/", "/api/users", "/api/a/b"] {
            assert_eq!(names(&routes, &router, None, path), "api", "{path}");
        }
        for path in ["/", "/apix", "/ap", "/API", ""] {
            assert_eq!(names(&routes, &router, None, path), "-", "{path}");
        }
    }

    #[test]
    fn a_trailing_slash_in_the_prefix_is_the_same_prefix() {
        let routes = vec![route("api", "", "/api/")];
        let router = Router::new(&routes);
        assert_eq!(names(&routes, &router, None, "/api"), "api");
        assert_eq!(names(&routes, &router, None, "/api/x"), "api");
        assert_eq!(names(&routes, &router, None, "/apix"), "-");
    }

    #[test]
    fn the_longest_prefix_wins() {
        let routes = vec![
            route("root", "", "/"),
            route("api", "", "/api"),
            route("v2", "", "/api/v2"),
        ];
        let router = Router::new(&routes);
        assert_eq!(names(&routes, &router, None, "/"), "root");
        assert_eq!(names(&routes, &router, None, "/other"), "root");
        assert_eq!(names(&routes, &router, None, "/api/v1"), "api");
        assert_eq!(names(&routes, &router, None, "/api/v2/x"), "v2");
        assert_eq!(names(&routes, &router, None, "/api/v20"), "api");
    }

    #[test]
    fn a_host_route_wins_over_an_any_host_route() {
        let routes = vec![
            route("any", "", "/api"),
            route("exact", "api.example.com", "/api"),
            route("wild", "*.example.com", "/api"),
        ];
        let router = Router::new(&routes);
        assert_eq!(
            names(&routes, &router, Some("api.example.com"), "/api"),
            "exact"
        );
        assert_eq!(
            names(&routes, &router, Some("API.Example.COM:8080"), "/api"),
            "exact"
        );
        assert_eq!(
            names(&routes, &router, Some("api.example.com."), "/api"),
            "exact"
        );
        assert_eq!(
            names(&routes, &router, Some("x.example.com"), "/api"),
            "wild"
        );
        assert_eq!(
            names(&routes, &router, Some("a.b.example.com"), "/api"),
            "wild"
        );
        assert_eq!(names(&routes, &router, Some("example.com"), "/api"), "any");
        assert_eq!(names(&routes, &router, Some("other.org"), "/api"), "any");
        assert_eq!(names(&routes, &router, None, "/api"), "any");
    }

    #[test]
    fn a_longer_prefix_wins_over_a_host() {
        let routes = vec![route("host", "a.com", "/"), route("deep", "", "/x/y")];
        let router = Router::new(&routes);
        assert_eq!(names(&routes, &router, Some("a.com"), "/x/y/z"), "deep");
        assert_eq!(names(&routes, &router, Some("a.com"), "/x"), "host");
    }

    #[test]
    fn a_host_route_does_not_match_other_hosts() {
        let routes = vec![route("wild", "*.example.com", "/")];
        let router = Router::new(&routes);
        assert_eq!(names(&routes, &router, Some("example.com"), "/"), "-");
        assert_eq!(names(&routes, &router, Some("badexample.com"), "/"), "-");
        assert_eq!(names(&routes, &router, None, "/"), "-");
        assert_eq!(names(&routes, &router, Some("[::1]:80"), "/"), "-");
    }

    #[test]
    fn ipv6_hosts_lose_the_port_only() {
        let routes = vec![route("v6", "[::1]", "/")];
        let router = Router::new(&routes);
        assert_eq!(names(&routes, &router, Some("[::1]:8080"), "/"), "v6");
        assert_eq!(names(&routes, &router, Some("[::1]"), "/"), "v6");
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
    fn dot_segments_are_found() {
        for path in [
            "/a/../b",
            "/..",
            "/.",
            "/a/./b",
            "/a/%2e%2e/b",
            "/a/%2E./b",
            "/a/.%2e",
            "/a/..",
        ] {
            assert!(has_dot_segment(path), "{path}");
        }
        for path in ["/", "/a/b", "/a..b", "/.well-known/x", "/a/...", "/a/b.c"] {
            assert!(!has_dot_segment(path), "{path}");
        }
    }

    proptest! {
        /// A match always has a prefix that is a segment prefix of the path.
        #[test]
        fn a_match_is_a_segment_prefix(
            prefixes in proptest::collection::vec("(/[a-c]{1,2}){0,3}", 1..6),
            path in "(/[a-c]{0,2}){0,4}",
        ) {
            let routes: Vec<RouteConfig> = prefixes
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
