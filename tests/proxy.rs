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
            .configure(|c| c.trust_forwarded_headers = true)
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
    let stream = tokio::net::TcpStream::connect(handle.local_addr().unwrap())
        .await
        .unwrap();
    let response = raw(
        stream,
        "GET /a/../admin HTTP/1.1\r\nhost: x\r\nconnection: close\r\n\r\n",
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 400"), "{response}");
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

    let chunks: Vec<Result<Vec<u8>, std::io::Error>> = (0..8).map(|_| Ok(vec![1u8; 512])).collect();
    let stream = futures_util::stream::iter(chunks);
    let response = client()
        .post(url(&handle, "/up"))
        .body(reqwest::Body::wrap_stream(stream))
        .send()
        .await;
    match response {
        Ok(response) => assert_eq!(response.status().as_u16(), 413, "streamed body"),
        // The proxy can close the connection before the client ends the body.
        Err(error) => assert!(!error.is_timeout(), "{error}"),
    }
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

/// Write `request` and read until the server closes the connection.
async fn raw(mut stream: tokio::net::TcpStream, request: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stream.read_to_end(&mut buf),
    )
    .await;
    String::from_utf8_lossy(&buf).into_owned()
}
