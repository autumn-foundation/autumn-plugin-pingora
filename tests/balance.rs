//! Load balancing, health checks, retries and timeouts: AC4 and AC5.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

mod common;

use std::collections::HashMap;

use autumn_plugin_pingora::{Route, Selection};
use common::{boot, dead_address, eventually, get, plugin, route, upstream};

async fn names(handle: &autumn_plugin_pingora::PingoraHandle, n: usize) -> HashMap<String, usize> {
    let mut seen = HashMap::new();
    for _ in 0..n {
        let (status, body) = get(handle, "/").await;
        assert_eq!(status, 200, "{body}");
        *seen
            .entry(body["name"].as_str().unwrap().to_owned())
            .or_default() += 1;
    }
    seen
}

#[tokio::test(flavor = "multi_thread")]
async fn round_robin_uses_each_upstream() {
    let a = upstream("a").await;
    let b = upstream("b").await;
    let (_http, handle) = boot(plugin().route(route("r", "/", &[&a, &b])));
    let seen = names(&handle, 10).await;
    assert_eq!(seen.get("a"), Some(&5), "{seen:?}");
    assert_eq!(seen.get("b"), Some(&5), "{seen:?}");
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn consistent_selection_keeps_a_client_on_one_upstream() {
    let a = upstream("a").await;
    let b = upstream("b").await;
    let (_http, handle) =
        boot(plugin().route(route("r", "/", &[&a, &b]).selection(Selection::Consistent)));
    let seen = names(&handle, 10).await;
    assert_eq!(seen.len(), 1, "{seen:?}");
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn health_checks_remove_a_dead_upstream() {
    let a = upstream("a").await;
    let dead = dead_address();
    let (_http, handle) = boot(
        plugin()
            .configure(|c| c.max_retries = 0)
            .route(Route::new("r").upstream(a.address()).upstream(dead)),
    );
    let probe = handle.clone();
    assert!(
        eventually(move || {
            let probe = probe.clone();
            async move { probe.upstream_health() == vec![("r".to_owned(), 1, 2)] }
        })
        .await,
        "{:?}",
        handle.upstream_health()
    );
    let seen = names(&handle, 6).await;
    assert_eq!(seen.get("a"), Some(&6), "{seen:?}");
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn no_healthy_upstream_gives_503() {
    let (_http, handle) = boot(plugin().route(Route::new("r").upstream(dead_address())));
    let probe = handle.clone();
    assert!(
        eventually(move || {
            let probe = probe.clone();
            async move { probe.upstream_health() == vec![("r".to_owned(), 0, 1)] }
        })
        .await
    );
    let (status, _) = get(&handle, "/").await;
    assert_eq!(status, 503);
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_connect_failure_retries_on_another_upstream() {
    let a = upstream("a").await;
    let (_http, handle) = boot(
        plugin()
            .configure(|c| {
                c.health_check_interval_ms = 0;
                c.max_retries = 1;
            })
            .route(
                Route::new("r")
                    .upstream(dead_address())
                    .upstream(a.address()),
            ),
    );
    let seen = names(&handle, 6).await;
    assert_eq!(seen.get("a"), Some(&6), "{seen:?}");
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn without_retries_a_connect_failure_gives_502() {
    let (_http, handle) = boot(
        plugin()
            .configure(|c| {
                c.health_check_interval_ms = 0;
                c.max_retries = 0;
            })
            .route(Route::new("r").upstream(dead_address())),
    );
    let (status, _) = get(&handle, "/").await;
    assert_eq!(status, 502);
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slow_upstream_gives_504() {
    let a = upstream("a").await;
    let (_http, handle) = boot(plugin().configure(|c| c.read_timeout_ms = 200).route(route(
        "r",
        "/",
        &[&a],
    )));
    let (status, _) = get(&handle, "/slow/2000").await;
    assert_eq!(status, 504);
    let (status, _) = get(&handle, "/slow/10").await;
    assert_eq!(status, 200);
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn upstream_host_names_resolve_at_boot() {
    let a = upstream("a").await;
    let (_http, handle) =
        boot(plugin().route(Route::new("r").upstream(format!("localhost:{}", a.addr.port()))));
    let (status, body) = get(&handle, "/").await;
    assert_eq!(status, 200, "{body}");
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_upstream_host_aborts_boot() {
    let plugin = plugin().route(Route::new("r").upstream("no-such-host.invalid:80"));
    let message = common::boot_error(autumn_web::test::TestApp::new, plugin);
    assert!(message.contains("no-such-host.invalid"), "{message}");
}
