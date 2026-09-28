//! Framework fit: AC11.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

mod common;

use autumn_plugin_pingora::{Fallback, PLUGIN_NAME, PingoraPlugin, Route};
use autumn_web::plugin::Plugin;
use autumn_web::plugin_conformance::{ConformanceConfig, run_conformance};
use autumn_web::route_listing::{RouteInfo, RouteSource};

fn manifest(plugin: &PingoraPlugin) -> Vec<RouteInfo> {
    let name = plugin.name().into_owned();
    plugin
        .route_infos()
        .into_iter()
        .map(|mut route| {
            route.source = RouteSource::Plugin(name.clone());
            route
        })
        .collect()
}

fn listed(routes: &[RouteInfo]) -> Vec<String> {
    let mut listed: Vec<String> = routes
        .iter()
        .map(|r| format!("{} {} {}", r.method, r.path, r.classification.as_str()))
        .collect();
    listed.sort();
    listed
}

fn sample() -> PingoraPlugin {
    common::plugin()
        .route(
            Route::new("api")
                .path_prefix("/api")
                .upstream("127.0.0.1:9"),
        )
        .route(
            Route::new("docs")
                .host("docs.example.com")
                .upstream("127.0.0.1:9"),
        )
}

#[test]
fn passes_the_framework_conformance_harness() {
    let plugin = sample();
    let name = plugin.name().into_owned();
    assert_eq!(name, PLUGIN_NAME);
    let routes = manifest(&plugin);
    let report = run_conformance(&ConformanceConfig::new(&name), &routes);
    assert!(report.passed(), "{}", report.to_text_report());
    assert_eq!(
        listed(&routes),
        [
            "PROXY /api/* unclassified",
            "PROXY docs.example.com/* unclassified"
        ]
    );
}

#[test]
fn public_marks_the_proxied_prefixes_public() {
    let routes = manifest(&sample().public());
    assert_eq!(
        listed(&routes),
        ["PROXY /api/* public", "PROXY docs.example.com/* public"]
    );
}

#[test]
fn disabled_or_invalid_plugins_declare_nothing() {
    assert!(
        sample()
            .configure(|c| c.enabled = false)
            .route_infos()
            .is_empty()
    );
    assert!(
        sample()
            .configure(|c| c.shutdown_grace_ms = 0)
            .route_infos()
            .is_empty()
    );
}

#[test]
fn the_fallback_is_not_declared() {
    let plugin = common::plugin().fallback(Fallback::App);
    assert!(plugin.route_infos().is_empty());
}

#[test]
fn builders_change_the_effective_config() {
    let plugin = common::plugin()
        .bind("127.0.0.1:7777")
        .fallback(Fallback::None)
        .route(Route::new("a").upstream("127.0.0.1:1"));
    let config = plugin.effective_config().unwrap();
    assert_eq!(config.bind, "127.0.0.1:7777");
    assert_eq!(config.routes.len(), 1);
    assert!(format!("{plugin:?}").contains("PingoraPlugin"));
}
