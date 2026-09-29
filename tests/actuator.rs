//! Autumn health indicator and metrics: AC8.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

mod common;

use autumn_web::config::AutumnConfig;
use autumn_web::test::TestApp;
use common::{boot_with, dead, eventually, get, has_line, plugin, route, upstream};

fn detailed() -> TestApp {
    let mut config = AutumnConfig::default();
    config.health.detailed = true;
    TestApp::new().config(config)
}

/// Find a named health component, wherever this Autumn version puts it.
fn component<'a>(body: &'a serde_json::Value, name: &str) -> Option<&'a serde_json::Value> {
    ["components", "checks"]
        .iter()
        .find_map(|key| body.get(key).and_then(|c| c.get(name)))
}

#[tokio::test(flavor = "multi_thread")]
async fn health_is_up_while_serving_and_down_after_shutdown() {
    let a = upstream("a").await;
    let (http, handle) = boot_with(detailed(), plugin().route(route("a", "/", &[&a])));
    let body: serde_json::Value = http.get("/actuator/health").send().await.json();
    let pingora = component(&body, "pingora").unwrap_or_else(|| panic!("no component: {body}"));
    assert_eq!(pingora["status"], "UP", "{body}");
    assert_eq!(pingora["details"]["state"], "serving");
    assert_eq!(
        pingora["details"]["address"],
        handle.local_addr().unwrap().to_string().as_str()
    );
    assert_eq!(pingora["details"]["routes"]["a"], "1/1", "{body}");

    handle.shutdown().await;
    let body: serde_json::Value = http.get("/actuator/health").send().await.json();
    let pingora = component(&body, "pingora").unwrap();
    assert_eq!(pingora["status"], "DOWN", "{body}");
    assert_eq!(pingora["details"]["state"], "stopped");
}

#[tokio::test(flavor = "multi_thread")]
async fn metrics_count_requests_by_route_and_status_class() {
    let a = upstream("a").await;
    let dead = dead();
    let (http, handle) = boot_with(
        TestApp::new(),
        plugin()
            .route(route("a", "/a", &[&a]).strip_prefix(true))
            .route(
                autumn_plugin_pingora::Route::new("dead")
                    .path_prefix("/dead")
                    .upstream(dead.address()),
            ),
    );
    get(&handle, "/a/1").await;
    get(&handle, "/a/2").await;
    get(&handle, "/a/status/500").await;
    get(&handle, "/nothing").await;
    let probe = handle.clone();
    assert!(
        eventually(move || {
            let probe = probe.clone();
            async move { common::health(&probe).contains(&("dead".to_owned(), 0, 1)) }
        })
        .await
    );
    get(&handle, "/dead").await;

    let text = http.get("/actuator/prometheus").send().await.text();
    for line in [
        r#"pingora_proxy_requests_total{route="a",status="2xx"} 2"#,
        r#"pingora_proxy_requests_total{route="a",status="5xx"} 1"#,
        r#"pingora_proxy_requests_total{route="unmatched",status="4xx"} 1"#,
        r#"pingora_proxy_requests_total{route="dead",status="5xx"} 1"#,
        r#"pingora_proxy_upstream_errors_total{route="dead"} 1"#,
        r#"pingora_proxy_upstreams_healthy{route="a"} 1"#,
        r#"pingora_proxy_upstreams_healthy{route="dead"} 0"#,
        r#"pingora_proxy_upstreams{route="a"} 1"#,
        "pingora_proxy_up 1",
    ] {
        assert!(has_line(&text, line), "missing `{line}` in:\n{text}");
    }
    assert!(text.contains("pingora_proxy_connections_active"), "{text}");
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn metrics_can_be_turned_off() {
    let a = upstream("a").await;
    let (http, handle) = boot_with(
        TestApp::new(),
        plugin()
            .configure(|c| c.metrics = false)
            .route(route("a", "/", &[&a])),
    );
    get(&handle, "/").await;
    let text = http.get("/actuator/prometheus").send().await.text();
    assert!(!text.contains("pingora_proxy_requests_total"), "{text}");
    handle.shutdown().await;
}
