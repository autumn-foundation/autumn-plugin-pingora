//! Routing, headers and limits: AC1, AC3, AC6 and AC7.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

mod common;

use autumn_plugin_pingora::{Lifecycle, Route};
use common::{boot, client, get, plugin, route, send, upstream, url};

#[tokio::test(flavor = "multi_thread")]
async fn routes_by_prefix_and_strips_the_prefix() {
    let a = upstream("a").await;
    let b = upstream("b").await;
    let (_http, handle) = boot(
        plugin()
            .route(route("a", "/a", &[&a]).strip_prefix(true))
            .route(route("b", "/b", &[&b])),
    );
    assert_eq!(handle.state(), Lifecycle::Serving);

    let (status, body) = get(&handle, "/a/x?q=1").await;
    assert_eq!(status, 200);
    assert_eq!(body["name"], "a");
    assert_eq!(body["target"], "/x?q=1");

    let (_, body) = get(&handle, "/a").await;
    assert_eq!(body["target"], "/");

    let (_, body) = get(&handle, "/b/x").await;
    assert_eq!(body["name"], "b");
    assert_eq!(body["target"], "/b/x");

    let (status, _) = get(&handle, "/ab").await;
    assert_eq!(status, 404, "segment boundary");
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unmatched_requests_get_404_with_fallback_none() {
    let a = upstream("a").await;
    let (_http, handle) = boot(plugin().route(route("a", "/a", &[&a])));
    let (status, _) = get(&handle, "/nothing/here").await;
    assert_eq!(status, 404);
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn routes_by_host() {
    let any = upstream("any").await;
    let api = upstream("api").await;
    let wild = upstream("wild").await;
    let (_http, handle) = boot(
        plugin()
            .route(route("any", "/", &[&any]))
            .route(route("api", "/", &[&api]).host("api.example.com"))
            .route(route("wild", "/", &[&wild]).host("*.example.com")),
    );
    for (host, want) in [
        ("api.example.com", "api"),
        ("API.example.com:8080", "api"),
        ("shop.example.com", "wild"),
        ("example.org", "any"),
    ] {
        let (_, body) = send(client().get(url(&handle, "/")).header("host", host)).await;
        assert_eq!(body["name"], want, "{host}");
    }
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sets_forwarded_headers_and_replaces_client_values() {
    let a = upstream("a").await;
    let (_http, handle) = boot(plugin().route(route("a", "/", &[&a])));
    let (_, body) = send(
        client()
            .get(url(&handle, "/"))
            .header("host", "shop.example")
            .header("x-forwarded-for", "1.2.3.4")
            .header("x-forwarded-proto", "https")
            .header("forwarded", "for=1.2.3.4"),
    )
    .await;
    let headers = &body["headers"];
    assert_eq!(headers["x-forwarded-for"], "127.0.0.1");
    assert_eq!(headers["x-forwarded-proto"], "http");
    assert_eq!(headers["x-forwarded-host"], "shop.example");
    assert_eq!(headers["host"], "shop.example", "Host is kept");
    assert!(headers.get("forwarded").is_none(), "{headers}");
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn trusted_forwarded_headers_are_kept() {
    let a = upstream("a").await;
    let (_http, handle) = boot(
        plugin()
            .configure(|c| c.trusted_proxies = vec!["127.0.0.0/8".to_owned()])
            .route(route("a", "/", &[&a])),
    );
    let (_, body) = send(
        client()
            .get(url(&handle, "/"))
            .header("x-forwarded-for", "1.2.3.4")
            .header("x-forwarded-proto", "https"),
    )
    .await;
    assert_eq!(body["headers"]["x-forwarded-for"], "1.2.3.4, 127.0.0.1");
    assert_eq!(body["headers"]["x-forwarded-proto"], "https");
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn peers_outside_trusted_proxies_are_not_trusted() {
    let a = upstream("a").await;
    let (_http, handle) = boot(
        plugin()
            .configure(|c| c.trusted_proxies = vec!["10.0.0.0/8".to_owned()])
            .route(route("a", "/", &[&a])),
    );
    let (_, body) = send(
        client()
            .get(url(&handle, "/"))
            .header("x-forwarded-for", "1.2.3.4")
            .header("x-real-ip", "10.0.0.1")
            .header("x-forwarded-prefix", "/admin"),
    )
    .await;
    let headers = &body["headers"];
    assert_eq!(headers["x-forwarded-for"], "127.0.0.1");
    assert!(headers.get("x-real-ip").is_none(), "{headers}");
    assert!(headers.get("x-forwarded-prefix").is_none(), "{headers}");
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn upstream_host_replaces_the_host_header() {
    let a = upstream("a").await;
    let (_http, handle) =
        boot(plugin().route(route("a", "/", &[&a]).upstream_host("internal.svc")));
    let (_, body) = send(
        client()
            .get(url(&handle, "/"))
            .header("host", "public.example"),
    )
    .await;
    assert_eq!(body["headers"]["host"], "internal.svc");
    assert_eq!(body["headers"]["x-forwarded-host"], "public.example");
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn dot_segments_are_rejected() {
    let a = upstream("a").await;
    let (_http, handle) = boot(plugin().route(route("a", "/a", &[&a])));
    for target in [
        "/a/../admin",
        "/a/..;/admin",
        "/a/..%2fadmin",
        "/a/%252e%252e/admin",
        "/a/..%5cadmin",
    ] {
        let request = format!("GET {target} HTTP/1.1\r\nhost: x\r\nconnection: close\r\n\r\n");
        let response = common::raw(&handle, request.as_bytes()).await;
        assert!(response.starts_with("HTTP/1.1 400"), "{target}: {response}");
    }
    let (status, _) = get(&handle, "/a/files/x%2Fy").await;
    assert_eq!(status, 200, "an encoded slash alone is fine");
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn bodies_pass_through_and_big_bodies_get_413() {
    let a = upstream("a").await;
    let (_http, handle) = boot(
        plugin()
            .configure(|c| c.max_request_body_bytes = 1024)
            .route(route("a", "/", &[&a])),
    );
    let (status, body) = send(client().post(url(&handle, "/up")).body(vec![7u8; 1000])).await;
    assert_eq!(status, 200);
    assert_eq!(body["method"], "POST");
    assert_eq!(body["body_len"], 1000);

    let (status, _) = send(client().post(url(&handle, "/up")).body(vec![7u8; 2000])).await;
    assert_eq!(status, 413, "Content-Length over the limit");

    let chunk = "7".repeat(1000);
    let request = format!(
        "POST /up HTTP/1.1\r\nhost: x\r\ntransfer-encoding: chunked\r\n\r\n3e8\r\n{chunk}\r\n3e8\r\n{chunk}\r\n0\r\n\r\n"
    );
    let response = common::raw(&handle, request.as_bytes()).await;
    assert!(
        response.starts_with("HTTP/1.1 413"),
        "streamed body: {response}"
    );
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn upstream_status_codes_pass_through() {
    let a = upstream("a").await;
    let (_http, handle) = boot(plugin().route(route("a", "/", &[&a])));
    let (status, _) = get(&handle, "/status/418").await;
    assert_eq!(status, 418);
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_route_from_config_works_like_a_route_from_code() {
    let a = upstream("a").await;
    let address = a.address();
    let (_http, handle) = boot(plugin().configure(move |c| {
        c.routes.push(
            Route::new("cfg")
                .path_prefix("/cfg")
                .upstream(address.clone()),
        );
    }));
    let (_, body) = get(&handle, "/cfg/x").await;
    assert_eq!(body["name"], "a");
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn trusted_forwarded_host_is_kept() {
    let a = upstream("a").await;
    let (_http, handle) = boot(
        plugin()
            .configure(|c| c.trusted_proxies = vec!["127.0.0.1".to_owned()])
            .route(route("a", "/", &[&a])),
    );
    let (_, body) = send(
        client()
            .get(url(&handle, "/"))
            .header("host", "internal")
            .header("x-forwarded-host", "public.example"),
    )
    .await;
    assert_eq!(body["headers"]["x-forwarded-host"], "public.example");
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn http_1_0_without_host_gets_a_host_upstream() {
    let a = upstream("a").await;
    let (_http, handle) = boot(plugin().route(route("a", "/", &[&a])));
    let response = common::raw(&handle, b"GET /x HTTP/1.0\r\n\r\n").await;
    assert!(response.contains(" 200 "), "{response}");
    let body = &response[response.find('{').unwrap()..];
    let body: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(body["headers"]["host"], a.address().as_str());
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn keep_alive_connections_are_reused() {
    let a = upstream("a").await;
    let (_http, handle) = boot(plugin().route(route("a", "/", &[&a])));
    let pooled = reqwest::Client::new();
    for _ in 0..3 {
        let (status, _) = send(pooled.get(url(&handle, "/"))).await;
        assert_eq!(status, 200);
    }
    assert_eq!(handle.active_connections(), 1, "one reused connection");
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn max_connections_limits_open_connections() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let a = upstream("a").await;
    let (_http, handle) = boot(plugin().configure(|c| c.max_connections = 1).route(route(
        "a",
        "/",
        &[&a],
    )));
    let slow = {
        let handle = handle.clone();
        tokio::spawn(async move { common::get(&handle, "/slow/800").await })
    };
    let probe = handle.clone();
    assert!(
        common::eventually(move || {
            let probe = probe.clone();
            async move { probe.active_connections() == 1 }
        })
        .await
    );
    let mut second = tokio::net::TcpStream::connect(handle.local_addr().unwrap())
        .await
        .unwrap();
    second
        .write_all(b"GET / HTTP/1.1\r\nhost: x\r\nconnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut buf = [0u8; 16];
    let early =
        tokio::time::timeout(std::time::Duration::from_millis(300), second.read(&mut buf)).await;
    assert!(early.is_err(), "no answer while the slot is taken");
    assert_eq!(handle.active_connections(), 1);
    assert_eq!(slow.await.unwrap().0, 200);
    let mut rest = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        second.read_to_end(&mut rest),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(String::from_utf8_lossy(&rest).starts_with("HTTP/1.1 200"));
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn max_connections_per_ip_closes_extra_connections() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let a = upstream("a").await;
    let (_http, handle) = boot(
        plugin()
            .configure(|c| c.max_connections_per_ip = 1)
            .route(route("a", "/", &[&a])),
    );
    let first = tokio::net::TcpStream::connect(handle.local_addr().unwrap())
        .await
        .unwrap();
    let probe = handle.clone();
    assert!(
        common::eventually(move || {
            let probe = probe.clone();
            async move { probe.active_connections() == 1 }
        })
        .await
    );
    let mut second = tokio::net::TcpStream::connect(handle.local_addr().unwrap())
        .await
        .unwrap();
    let _ = second.write_all(b"GET / HTTP/1.1\r\nhost: x\r\n\r\n").await;
    let mut buf = Vec::new();
    let read = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        second.read_to_end(&mut buf),
    )
    .await
    .expect("the proxy closes the extra connection");
    assert!(read.is_err() || buf.is_empty(), "no response: {buf:?}");
    drop(first);
    let probe = handle.clone();
    assert!(
        common::eventually(move || {
            let probe = probe.clone();
            async move { probe.active_connections() == 0 }
        })
        .await
    );
    let (status, _) = get(&handle, "/").await;
    assert_eq!(status, 200, "the slot is free again");
    handle.shutdown().await;
}
