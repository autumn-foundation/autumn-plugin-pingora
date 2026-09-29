//! Load balancing, health checks, retries and timeouts: AC4 and AC5.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

mod common;

use std::collections::HashMap;

use autumn_plugin_pingora::{Route, Selection};
use common::{boot, dead, eventually, get, plugin, route, upstream};

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
    // `upstream::tests` checks that different keys spread.
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
    let dead = dead();
    let (_http, handle) = boot(
        plugin().configure(|c| c.max_retries = 0).route(
            Route::new("r")
                .upstream(a.address())
                .upstream(dead.address()),
        ),
    );
    let probe = handle.clone();
    assert!(
        eventually(move || {
            let probe = probe.clone();
            async move { common::health(&probe) == vec![("r".to_owned(), 1, 2)] }
        })
        .await,
        "{:?}",
        common::health(&handle)
    );
    let seen = names(&handle, 6).await;
    assert_eq!(seen.get("a"), Some(&6), "{seen:?}");
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn no_healthy_upstream_gives_503() {
    let dead = dead();
    let (_http, handle) = boot(plugin().route(Route::new("r").upstream(dead.address())));
    let probe = handle.clone();
    assert!(
        eventually(move || {
            let probe = probe.clone();
            async move { common::health(&probe) == vec![("r".to_owned(), 0, 1)] }
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
    let dead = dead();
    let (_http, handle) = boot(
        plugin()
            .configure(|c| {
                c.health_check_interval_ms = 0;
                c.max_retries = 1;
            })
            .route(
                Route::new("r")
                    .upstream(dead.address())
                    .upstream(a.address()),
            ),
    );
    let seen = names(&handle, 6).await;
    assert_eq!(seen.get("a"), Some(&6), "{seen:?}");
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn without_retries_a_connect_failure_gives_502() {
    let dead = dead();
    let (_http, handle) = boot(
        plugin()
            .configure(|c| {
                c.health_check_interval_ms = 0;
                c.max_retries = 0;
            })
            .route(Route::new("r").upstream(dead.address())),
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
    assert_eq!(body["name"], "a");
    let resolved = tokio::net::lookup_host(("localhost", a.addr.port()))
        .await
        .unwrap()
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    assert_eq!(
        handle.upstream_health()[0].total,
        resolved,
        "one upstream per address"
    );
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn health_checks_can_be_turned_off() {
    let dead = dead();
    let (_http, handle) = boot(
        plugin()
            .configure(|c| c.health_check_interval_ms = 0)
            .route(Route::new("r").upstream(dead.address())),
    );
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(common::health(&handle), vec![("r".to_owned(), 1, 1)]);
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn retries_stop_at_max_retries() {
    let a = upstream("a").await;
    let (first, second) = (dead(), dead());
    let (http, handle) = common::boot_with(
        autumn_web::test::TestApp::new(),
        plugin()
            .configure(|c| {
                c.health_check_interval_ms = 0;
                c.max_retries = 1;
            })
            .route(
                Route::new("r")
                    .upstream(first.address())
                    .upstream(second.address())
                    .upstream(a.address()),
            ),
    );
    let mut failures = 0;
    for _ in 0..6 {
        let (status, _) = get(&handle, "/").await;
        if status == 502 {
            failures += 1;
        } else {
            assert_eq!(status, 200);
        }
    }
    assert!(failures > 0, "two dead upstreams need two retries");
    let text = http.get("/actuator/prometheus").send().await.text();
    let errors = text
        .lines()
        .find(|l| l.starts_with(r#"pingora_proxy_upstream_errors_total{route="r"}"#))
        .and_then(|l| l.rsplit(' ').next())
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap();
    assert!(
        errors >= failures * 2,
        "{errors} errors for {failures} failures"
    );
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_upstream_host_aborts_boot() {
    let plugin = plugin().route(Route::new("r").upstream("no-such-host.invalid:80"));
    let message = common::boot_error(autumn_web::test::TestApp::new, plugin);
    assert!(message.contains("no-such-host.invalid"), "{message}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_single_dead_upstream_gives_502_with_default_retries() {
    let dead = dead();
    let (http, handle) = common::boot_with(
        autumn_web::test::TestApp::new(),
        plugin()
            .configure(|c| c.health_check_interval_ms = 0)
            .route(Route::new("r").upstream(dead.address())),
    );
    let (status, _) = get(&handle, "/").await;
    assert_eq!(status, 502, "not 503: the upstream was tried");
    let text = http.get("/actuator/prometheus").send().await.text();
    assert!(
        common::has_line(&text, r#"pingora_proxy_upstream_errors_total{route="r"} 1"#),
        "counted once:\n{text}"
    );
    handle.shutdown().await;
}
