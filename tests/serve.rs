//! Boot, handle and shutdown: AC8 and AC9.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use autumn_plugin_pingora::{Lifecycle, PingoraHandle};
use autumn_web::test::TestApp;
use common::{boot, eventually, get, plugin, route, upstream};

#[tokio::test(flavor = "multi_thread")]
async fn boot_binds_and_publishes_the_handle() {
    let seen = Arc::new(Mutex::new(None));
    let sink = seen.clone();
    let plugin = plugin();
    let handle = plugin.handle();
    assert_eq!(handle.state(), Lifecycle::Idle);
    assert!(handle.local_addr().is_none());
    let _http = TestApp::new()
        .plugin(plugin)
        .state_initializer(move |state| {
            let published = state.extension::<PingoraHandle>();
            *sink.lock().unwrap() = published.and_then(|h| h.local_addr());
        })
        .build();
    assert_eq!(handle.state(), Lifecycle::Serving);
    assert!(handle.local_addr().is_some());
    assert_eq!(*seen.lock().unwrap(), handle.local_addr());
    handle.shutdown().await;
    assert_eq!(handle.state(), Lifecycle::Stopped);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bind_failure_aborts_boot() {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = taken.local_addr().unwrap();
    let plugin = plugin().bind(addr.to_string());
    let handle = plugin.handle();
    let message = common::boot_error(TestApp::new, plugin);
    assert!(message.contains(&addr.to_string()), "{message}");
    assert_eq!(handle.state(), Lifecycle::Failed);
    drop(taken);
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_configuration_aborts_boot() {
    let plugin = plugin().configure(|c| c.shutdown_grace_ms = 0);
    let handle = plugin.handle();
    let message = common::boot_error(TestApp::new, plugin);
    assert!(message.contains("shutdown_grace_ms"), "{message}");
    assert_eq!(handle.state(), Lifecycle::Failed);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_disabled_plugin_binds_nothing() {
    let (_http, handle) = boot(plugin().configure(|c| c.enabled = false));
    assert_eq!(handle.state(), Lifecycle::Idle);
    assert!(handle.local_addr().is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_drains_open_requests_and_refuses_new_ones() {
    let a = upstream("a").await;
    let (_http, handle) = boot(plugin().route(route("a", "/", &[&a])));
    let addr = handle.local_addr().unwrap();
    let slow = {
        let handle = handle.clone();
        tokio::spawn(async move { get(&handle, "/slow/600").await })
    };
    let probe = handle.clone();
    assert!(
        eventually(move || {
            let probe = probe.clone();
            async move { probe.active_connections() == 1 }
        })
        .await
    );

    let stopper = handle.clone();
    let shutdown = tokio::spawn(async move { stopper.shutdown().await });
    let probe = handle.clone();
    assert!(
        eventually(move || {
            let probe = probe.clone();
            async move { probe.state() == Lifecycle::Draining }
        })
        .await
    );
    let refused =
        eventually(move || async move { tokio::net::TcpStream::connect(addr).await.is_err() })
            .await;
    assert!(refused, "no new connections during the drain");

    let (status, body) = slow.await.unwrap();
    assert_eq!(status, 200, "the open request finishes: {body}");
    tokio::time::timeout(Duration::from_secs(5), shutdown)
        .await
        .expect("the drain ends after the last request")
        .unwrap();
    assert_eq!(handle.state(), Lifecycle::Stopped);
    assert_eq!(handle.active_connections(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_grace_period_bounds_the_drain() {
    let a = upstream("a").await;
    let (_http, handle) = boot(
        plugin()
            .configure(|c| c.shutdown_grace_ms = 300)
            .route(route("a", "/", &[&a])),
    );
    let slow = {
        let handle = handle.clone();
        tokio::spawn(async move {
            common::client()
                .get(common::url(&handle, "/slow/5000"))
                .send()
                .await
        })
    };
    let probe = handle.clone();
    assert!(
        eventually(move || {
            let probe = probe.clone();
            async move { probe.active_connections() == 1 }
        })
        .await
    );
    let started = Instant::now();
    handle.shutdown().await;
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(handle.state(), Lifecycle::Stopped);
    assert!(slow.await.unwrap().is_err(), "the connection was closed");
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_is_idempotent_and_safe_before_boot() {
    let never = plugin().handle();
    never.shutdown().await;
    assert_eq!(never.state(), Lifecycle::Stopped);

    let (_http, handle) = boot(plugin());
    handle.shutdown().await;
    handle.shutdown().await;
    assert_eq!(handle.state(), Lifecycle::Stopped);
}

#[tokio::test(flavor = "multi_thread")]
async fn autumn_shutdown_signal_starts_the_drain() {
    let state = Arc::new(Mutex::new(None));
    let sink = state.clone();
    let plugin = plugin();
    let handle = plugin.handle();
    let _http = TestApp::new()
        .plugin(plugin)
        .state_initializer(move |s| *sink.lock().unwrap() = Some(s.clone()))
        .build();
    assert_eq!(handle.state(), Lifecycle::Serving);
    let app_state = state.lock().unwrap().clone().unwrap();
    app_state.trigger_shutdown_for_test();
    let probe = handle.clone();
    assert!(
        eventually(move || {
            let probe = probe.clone();
            async move { probe.state() == Lifecycle::Stopped }
        })
        .await,
        "{}",
        handle.state()
    );
}
