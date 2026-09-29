//! Fallback to the Autumn app: AC3 and AC10.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

mod common;

use autumn_plugin_pingora::Fallback;
use autumn_web::config::AutumnConfig;
use autumn_web::test::TestApp;
use common::{boot_with, get, plugin, route, upstream};

/// An Autumn config whose HTTP address is `upstream`: the test upstream
/// plays the Autumn app.
fn app_at(addr: std::net::SocketAddr) -> AutumnConfig {
    let mut config = AutumnConfig::default();
    "0.0.0.0".clone_into(&mut config.server.host);
    config.server.port = addr.port();
    config
}

#[tokio::test(flavor = "multi_thread")]
async fn unmatched_requests_go_to_the_app() {
    let app = upstream("app").await;
    let a = upstream("a").await;
    let (_http, handle) = boot_with(
        TestApp::new().config(app_at(app.addr)),
        plugin()
            .fallback(Fallback::App)
            .route(route("a", "/a", &[&a])),
    );
    let (_, body) = get(&handle, "/a/x").await;
    assert_eq!(body["name"], "a");
    let (status, body) = get(&handle, "/other?y=2").await;
    assert_eq!(status, 200);
    assert_eq!(body["name"], "app");
    assert_eq!(body["target"], "/other?y=2", "no strip for the fallback");
    assert_eq!(body["headers"]["x-forwarded-for"], "127.0.0.1");
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn zero_routes_give_an_edge_proxy_for_the_app() {
    let app = upstream("app").await;
    let (_http, handle) = boot_with(
        TestApp::new().config(app_at(app.addr)),
        plugin().fallback(Fallback::App),
    );
    let (status, body) = get(&handle, "/").await;
    assert_eq!(status, 200);
    assert_eq!(body["name"], "app");
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_proxy_on_the_app_address_aborts_boot() {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    drop(taken);
    let mut config = AutumnConfig::default();
    "127.0.0.1".clone_into(&mut config.server.host);
    config.server.port = port;
    let plugin = plugin()
        .fallback(Fallback::App)
        .bind(format!("127.0.0.1:{port}"));
    let message = common::boot_error(move || TestApp::new().config(config), plugin);
    assert!(message.contains("loop"), "{message}");
}

#[tokio::test(flavor = "multi_thread")]
async fn fallback_to_a_tls_app_aborts_boot() {
    let mut config = AutumnConfig::default();
    config.server.tls =
        Some(toml::from_str("cert_path = \"c.pem\"\nkey_path = \"k.pem\"").unwrap());
    let plugin = plugin().fallback(Fallback::App);
    let handle = plugin.handle();
    let message = common::boot_error(move || TestApp::new().config(config), plugin);
    assert!(message.contains("[server.tls]"), "{message}");
    assert_eq!(handle.state(), autumn_plugin_pingora::Lifecycle::Failed);
}
