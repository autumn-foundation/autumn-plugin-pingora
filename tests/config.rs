//! Configuration: AC1 and AC2.

use std::path::{Path, PathBuf};

use autumn_plugin_pingora::{Fallback, PingoraConfig, RouteConfig, Selection};
use autumn_web::config::MockEnv;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "autumn-pingora-config-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn env_for(dir: &Path) -> MockEnv {
    MockEnv::new().with("AUTUMN_MANIFEST_DIR", dir.to_str().unwrap())
}

fn route(name: &str, prefix: &str, upstream: &str) -> RouteConfig {
    let mut route = RouteConfig::default();
    name.clone_into(&mut route.name);
    prefix.clone_into(&mut route.path_prefix);
    route.upstreams = vec![upstream.to_owned()];
    route
}

fn edit(mut route: RouteConfig, change: impl FnOnce(&mut RouteConfig)) -> RouteConfig {
    change(&mut route);
    route
}

fn with_routes(routes: Vec<RouteConfig>) -> PingoraConfig {
    let mut config = PingoraConfig::default();
    config.routes = routes;
    config
}

#[test]
fn defaults_are_safe() {
    let config = PingoraConfig::default();
    assert!(config.enabled);
    assert_eq!(config.bind, "", "the profile decides");
    assert_eq!(
        config.bind_addr(true).unwrap().to_string(),
        "127.0.0.1:8080"
    );
    assert_eq!(config.bind_addr(false).unwrap().to_string(), "0.0.0.0:8080");
    assert_eq!(config.fallback, Fallback::App);
    assert!(!config.trust_forwarded_headers);
    assert!(config.connect_timeout_ms > 0);
    assert!(config.read_timeout_ms > 0);
    assert!(config.write_timeout_ms > 0);
    assert!(config.max_request_body_bytes > 0);
    assert!(config.max_connections > 0);
    assert_eq!(config.shutdown_grace_ms, 10_000);
    assert!(config.routes.is_empty());
    config.validate().unwrap();
}

#[test]
fn reads_the_section_and_routes_from_toml() {
    let text = r#"
        [pingora]
        bind = "127.0.0.1:9000"
        fallback = "none"
        trust_forwarded_headers = true
        connect_timeout_ms = 250
        max_retries = 3

        [[pingora.routes]]
        name = "billing"
        path_prefix = "/billing"
        upstreams = ["127.0.0.1:7001", "127.0.0.1:7002"]
        strip_prefix = true
        selection = "consistent"

        [[pingora.routes]]
        name = "docs"
        host = "*.docs.example.com"
        upstreams = ["localhost:7003"]
        upstream_host = "docs.internal"
    "#;
    let config = PingoraConfig::from_toml_str(text, "pingora").unwrap();
    assert_eq!(config.bind, "127.0.0.1:9000");
    assert_eq!(config.fallback, Fallback::None);
    assert!(config.trust_forwarded_headers);
    assert_eq!(config.connect_timeout_ms, 250);
    assert_eq!(config.max_retries, 3);
    assert_eq!(config.routes.len(), 2);
    let billing = &config.routes[0];
    assert_eq!(billing.name, "billing");
    assert_eq!(billing.upstreams.len(), 2);
    assert!(billing.strip_prefix);
    assert_eq!(billing.selection, Selection::Consistent);
    let docs = &config.routes[1];
    assert_eq!(docs.path_prefix, "/", "default prefix");
    assert_eq!(docs.host, "*.docs.example.com");
    assert_eq!(docs.upstream_host, "docs.internal");
    assert_eq!(docs.selection, Selection::RoundRobin);
}

#[test]
fn a_missing_section_gives_defaults() {
    let config = PingoraConfig::from_toml_str("[server]\nport = 3000", "pingora").unwrap();
    assert_eq!(config, PingoraConfig::default());
}

#[test]
fn unknown_keys_are_rejected() {
    let error = PingoraConfig::from_toml_str("[pingora]\nbindd = \"x\"", "pingora").unwrap_err();
    assert!(error.to_string().contains("bindd"), "{error}");
    let error = PingoraConfig::from_toml_str(
        "[[pingora.routes]]\nname = \"a\"\nupstreams = [\"127.0.0.1:1\"]\nprefix = \"/a\"",
        "pingora",
    )
    .unwrap_err();
    assert!(error.to_string().contains("prefix"), "{error}");
}

#[test]
fn bad_scalar_values_are_rejected() {
    for (text, needle) in [
        ("[pingora]\nbind = \"not an address\"", "bind"),
        ("[pingora]\nbind = \"localhost:80\"", "bind"),
        ("[pingora]\nshutdown_grace_ms = 0", "shutdown_grace_ms"),
        ("[pingora]\nconnect_timeout_ms = 0", "connect_timeout_ms"),
        (
            "[pingora]\nhealth_check_timeout_ms = 0",
            "health_check_timeout_ms",
        ),
        ("[pingora]\nfallback = \"maybe\"", "fallback"),
    ] {
        let error = PingoraConfig::from_toml_str(text, "pingora").unwrap_err();
        assert!(error.to_string().contains(needle), "{text}: {error}");
    }
}

#[test]
fn bad_routes_are_rejected() {
    let cases: Vec<(RouteConfig, &str)> = vec![
        (route("", "/a", "127.0.0.1:1"), "name"),
        (route("has space", "/a", "127.0.0.1:1"), "name"),
        (route("fallback", "/a", "127.0.0.1:1"), "reserved"),
        (route("unmatched", "/a", "127.0.0.1:1"), "reserved"),
        (route("a", "no-slash", "127.0.0.1:1"), "path_prefix"),
        (route("a", "/a?x", "127.0.0.1:1"), "path_prefix"),
        (route("a", "/a/../b", "127.0.0.1:1"), "path_prefix"),
        (route("a", "/a", "127.0.0.1"), "upstream"),
        (route("a", "/a", "127.0.0.1:notaport"), "upstream"),
        (route("a", "/a", ":80"), "upstream"),
        (route("a", "/a", "http://127.0.0.1:80"), "upstream"),
        (
            edit(route("a", "/a", "127.0.0.1:1"), |r| {
                r.upstreams = Vec::new()
            }),
            "upstreams",
        ),
        (
            edit(route("a", "/a", "127.0.0.1:1"), |r| {
                r.host = "bad host".to_owned()
            }),
            "host",
        ),
        (
            edit(route("a", "/a", "127.0.0.1:1"), |r| {
                r.host = "example.com:8080".to_owned()
            }),
            "host",
        ),
        (
            edit(route("a", "/a", "127.0.0.1:1"), |r| {
                r.host = "a.*.com".to_owned()
            }),
            "host",
        ),
        (
            edit(route("a", "/a", "127.0.0.1:1"), |r| {
                r.upstream_host = "evil\r\nx: y".to_owned()
            }),
            "upstream_host",
        ),
    ];
    for (bad, needle) in cases {
        let config = with_routes(vec![bad.clone()]);
        let error = config.validate().unwrap_err();
        assert!(
            error.message().contains(needle),
            "{bad:?}: expected `{needle}` in `{error}`"
        );
    }
}

#[test]
fn route_names_and_match_rules_must_be_unique() {
    let config = with_routes(vec![
        route("a", "/a", "127.0.0.1:1"),
        route("a", "/b", "127.0.0.1:1"),
    ]);
    assert!(config.validate().unwrap_err().message().contains("`a`"));

    let config = with_routes(vec![
        route("a", "/x", "127.0.0.1:1"),
        route("b", "/x/", "127.0.0.1:1"),
    ]);
    let error = config.validate().unwrap_err();
    assert!(error.message().contains("same"), "{error}");
}

#[test]
fn valid_routes_pass() {
    let mut docs = route("docs.v2", "/", "docs.internal:443");
    "*.example.com".clone_into(&mut docs.host);
    let config = with_routes(vec![
        route("api", "/api/", "127.0.0.1:1"),
        route("ipv6", "/v6", "[::1]:8080"),
        docs,
    ]);
    config.validate().unwrap();
}

#[test]
fn files_profiles_and_env_merge_in_order() {
    let dir = temp_dir("merge");
    std::fs::write(
        dir.join("autumn.toml"),
        r#"
            [pingora]
            bind = "127.0.0.1:9001"
            connect_timeout_ms = 100
            read_timeout_ms = 100

            [profile.prod.pingora]
            connect_timeout_ms = 200
        "#,
    )
    .unwrap();
    std::fs::write(
        dir.join("autumn-prod.toml"),
        "[pingora]\nread_timeout_ms = 300\nwrite_timeout_ms = 300\n",
    )
    .unwrap();
    let env = env_for(&dir)
        .with("AUTUMN_ENV", "prod")
        .with("AUTUMN_PINGORA__WRITE_TIMEOUT_MS", "400")
        .with("AUTUMN_PINGORA__FALLBACK", "none");
    let resolved = PingoraConfig::resolve_with_env("pingora", &env).unwrap();
    let config = resolved.config();
    assert_eq!(resolved.profile(), "prod");
    assert!(!resolved.is_development());
    assert_eq!(config.bind, "127.0.0.1:9001");
    assert_eq!(config.connect_timeout_ms, 200, "inline profile");
    assert_eq!(config.read_timeout_ms, 300, "profile file");
    assert_eq!(config.write_timeout_ms, 400, "env");
    assert_eq!(config.fallback, Fallback::None, "env enum");
}

#[test]
fn env_can_replace_the_routes() {
    let dir = temp_dir("routes-env");
    let env = env_for(&dir).with(
        "AUTUMN_PINGORA__ROUTES",
        r#"[{ name = "a", path_prefix = "/a", upstreams = ["127.0.0.1:1"] }]"#,
    );
    let resolved = PingoraConfig::resolve_with_env("pingora", &env).unwrap();
    assert!(resolved.is_development(), "dev is the default profile");
    assert_eq!(resolved.config().routes.len(), 1);
    assert_eq!(resolved.config().routes[0].name, "a");
}

#[test]
fn bad_env_values_are_errors() {
    let dir = temp_dir("bad-env");
    for (key, value) in [
        ("AUTUMN_PINGORA__CONNECT_TIMEOUT_MS", "soon"),
        ("AUTUMN_PINGORA__CONNECT_TIMEOUT_MS", "0"),
        ("AUTUMN_PINGORA__FALLBACK", "maybe"),
    ] {
        let env = env_for(&dir).with(key, value);
        let error = PingoraConfig::resolve_with_env("pingora", &env).unwrap_err();
        assert!(
            error.message().contains("CONNECT_TIMEOUT_MS")
                || error.message().contains("connect_timeout_ms")
                || error.message().contains("FALLBACK"),
            "{key}={value}: {error}"
        );
    }
}

#[test]
fn bad_files_are_errors() {
    let dir = temp_dir("bad-file");
    std::fs::write(dir.join("autumn.toml"), "[pingora\nbind = 1").unwrap();
    let error = PingoraConfig::resolve_with_env("pingora", &env_for(&dir)).unwrap_err();
    assert!(error.message().contains("autumn.toml"), "{error}");

    std::fs::write(dir.join("autumn.toml"), "pingora = 5").unwrap();
    let error = PingoraConfig::resolve_with_env("pingora", &env_for(&dir)).unwrap_err();
    assert!(error.message().contains("table"), "{error}");
}
