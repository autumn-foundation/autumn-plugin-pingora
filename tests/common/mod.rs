//! Shared test fixtures: echo upstreams, boot helpers and an HTTP client.

#![allow(
    dead_code,
    unused_imports,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    missing_docs
)]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::Duration;

use autumn_plugin_pingora::{Fallback, PingoraConfig, PingoraHandle, PingoraPlugin, Route};
use autumn_web::test::{TestApp, TestClient};
use axum::body::Bytes;
use axum::extract::{Path, Request};
use axum::http::HeaderMap;
use axum::routing::{self, any};
use serde_json::{Value, json};

/// A running echo upstream.
pub struct Upstream {
    pub addr: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl Upstream {
    /// `host:port` of the upstream.
    pub fn address(&self) -> String {
        self.addr.to_string()
    }

    /// Stop the upstream and close its listener.
    pub fn stop(self) {
        self.task.abort();
    }
}

/// An echo server on a free port. It answers every request with JSON:
/// `name`, `method`, `target`, `headers` and `body_len`. `/slow/{ms}`
/// waits before it answers. `/status/{code}` answers with `code`.
pub async fn upstream(name: &'static str) -> Upstream {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    upstream_on(name, listener)
}

/// Like [`upstream`], on `listener`.
pub fn upstream_on(name: &'static str, listener: tokio::net::TcpListener) -> Upstream {
    let addr = listener.local_addr().unwrap();
    let echo_all = move |request: Request| async move { echo(name, request).await };
    let app = axum::Router::new()
        .route(
            "/slow/{ms}",
            routing::get(move |Path(ms): Path<u64>, request: Request| async move {
                tokio::time::sleep(Duration::from_millis(ms)).await;
                echo(name, request).await
            }),
        )
        .route(
            "/status/{code}",
            routing::get(|Path(code): Path<u16>| async move {
                axum::http::StatusCode::from_u16(code).unwrap()
            }),
        )
        .fallback(any(echo_all));
    let task = tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });
    Upstream { addr, task }
}

async fn echo(name: &'static str, request: Request) -> axum::Json<Value> {
    let method = request.method().to_string();
    let target = request
        .uri()
        .path_and_query()
        .map_or_else(|| "/".to_owned(), ToString::to_string);
    let headers: BTreeMap<String, String> = request
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_owned()))
        .collect();
    let body = axum::body::to_bytes(request.into_body(), usize::MAX)
        .await
        .unwrap_or_default();
    axum::Json(json!({
        "name": name,
        "method": method,
        "target": target,
        "headers": headers,
        "body_len": body.len(),
    }))
}

/// A config that binds a free local port, reads no files and returns 404
/// for unmatched requests.
pub fn local_config() -> PingoraConfig {
    let mut config = PingoraConfig::default();
    "127.0.0.1:0".clone_into(&mut config.bind);
    config.fallback = Fallback::NotFound;
    config.shutdown_grace_ms = 2_000;
    config.health_check_interval_ms = 100;
    config.health_check_timeout_ms = 200;
    config.connect_timeout_ms = 500;
    config
}

/// A plugin with `local_config` and no routes.
pub fn plugin() -> PingoraPlugin {
    PingoraPlugin::new()
        .config(local_config())
        .development(true)
}

/// A route `name` for `prefix` to `upstreams`.
pub fn route(name: &str, prefix: &str, upstreams: &[&Upstream]) -> Route {
    Route::new(name)
        .path_prefix(prefix)
        .upstreams(upstreams.iter().map(|u| u.address()))
}

/// Boot `plugin` inside a `TestApp`.
pub fn boot(plugin: PingoraPlugin) -> (TestClient, PingoraHandle) {
    boot_with(TestApp::new(), plugin)
}

/// Boot `plugin` inside `app`.
pub fn boot_with(app: TestApp, plugin: PingoraPlugin) -> (TestClient, PingoraHandle) {
    let handle = plugin.handle();
    let client = app.plugin(plugin).build();
    (client, handle)
}

/// Boot and expect a startup failure. Returns the panic message.
pub fn boot_error(app: impl FnOnce() -> TestApp + Send + 'static, plugin: PingoraPlugin) -> String {
    let outcome = std::thread::spawn(move || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let _guard = runtime.enter();
            let _ = app().plugin(plugin).build();
        }))
    })
    .join()
    .unwrap();
    outcome
        .err()
        .and_then(|panic| {
            panic
                .downcast::<String>()
                .map(|s| *s)
                .or_else(|panic| panic.downcast::<&str>().map(|s| (*s).to_owned()))
                .ok()
        })
        .expect("boot must fail")
}

/// An HTTP client with no connection pool, so each request opens a new
/// connection.
pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

/// The proxy URL for `path`.
pub fn url(handle: &PingoraHandle, path: &str) -> String {
    format!(
        "http://{}{path}",
        handle.local_addr().expect("proxy is bound")
    )
}

/// GET `path` through the proxy. Returns the status and the JSON body
/// (`Null` when the body is not JSON).
pub async fn get(handle: &PingoraHandle, path: &str) -> (u16, Value) {
    send(client().get(url(handle, path))).await
}

/// Send `request`. Returns the status and the JSON body.
pub async fn send(request: reqwest::RequestBuilder) -> (u16, Value) {
    let response = request.send().await.expect("a response");
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

/// Poll `check` until it is true, for at most 5 s.
pub async fn eventually<F, Fut>(mut check: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..100 {
        if check().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

/// A local port that refuses connections. The socket is bound but does
/// not listen, so no other test can take the port while it lives.
pub struct Dead {
    addr: SocketAddr,
    _socket: socket2::Socket,
}

impl Dead {
    /// `host:port` of the dead port.
    pub fn address(&self) -> String {
        self.addr.to_string()
    }
}

/// A reserved local port that refuses connections.
pub fn dead() -> Dead {
    let socket = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None).unwrap();
    let any: SocketAddr = "127.0.0.1:0".parse().unwrap();
    socket.bind(&any.into()).unwrap();
    let addr = socket.local_addr().unwrap().as_socket().unwrap();
    Dead {
        addr,
        _socket: socket,
    }
}

/// `true` when `text` has `line` as a whole line.
pub fn has_line(text: &str, line: &str) -> bool {
    text.lines().any(|l| l == line)
}

/// Upstream health as `(route, healthy, total)` tuples.
pub fn health(handle: &PingoraHandle) -> Vec<(String, usize, usize)> {
    handle
        .upstream_health()
        .into_iter()
        .map(|h| (h.route, h.healthy, h.total))
        .collect()
}

/// Write `request` on a new connection to the proxy and read until the
/// server closes the connection (at most 5 s).
pub async fn raw(handle: &PingoraHandle, request: &[u8]) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(handle.local_addr().unwrap())
        .await
        .unwrap();
    stream.write_all(request).await.unwrap();
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut buf)).await;
    String::from_utf8_lossy(&buf).into_owned()
}
