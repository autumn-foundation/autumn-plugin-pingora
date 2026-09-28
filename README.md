# autumn-plugin-pingora

A [Pingora](https://github.com/cloudflare/pingora) reverse proxy for
[Autumn](https://autumn-web.app), in one line.

The proxy has its own port. It sends matched requests to upstream pools.
It sends all other requests to your Autumn app.

```rust
use autumn_plugin_pingora::{PingoraPlugin, Route};

#[autumn_web::main]
async fn main() {
    autumn_web::app()
        .routes(autumn_web::routes![index])
        .plugin(PingoraPlugin::new().route(
            Route::new("billing")
                .path_prefix("/billing")
                .upstreams(["10.0.0.7:8080", "10.0.0.8:8080"])
                .strip_prefix(true),
        ))
        .run()
        .await;
}
```

Run the example: `cargo run --example gateway`.

## Features

- Routes by path prefix (on a segment boundary) and host (`*.example.com`).
  The longest prefix wins.
- Fallback to the Autumn app, or 404.
- Round-robin or consistent-hash (client IP) load balancing.
- TCP health checks. No healthy upstream gives 503.
- Connect retries on another upstream. Timeouts give 504.
- `X-Forwarded-For`, `-Proto` and `-Host`. Client values are replaced
  unless you trust them.
- Body limit (413) and connection limit.
- A `pingora` indicator in `/actuator/health` (readiness group).
- `pingora_proxy_*` metrics in `/actuator/prometheus`.
- Graceful drain on Autumn's shutdown signal.
- `autumn routes` lists the proxied prefixes (method `PROXY`).

## Configuration

`[pingora]` in `autumn.toml`. Profiles (`[profile.prod.pingora]`,
`autumn-prod.toml`) and `AUTUMN_PINGORA__<KEY>` env variables apply on
top. Code (`configure`, `route`, `bind`) applies last. A bad value stops
boot.

```toml
[pingora]
enabled = true
bind = ""                          # "": 127.0.0.1:8080 in dev/test, 0.0.0.0:8080 else
fallback = "app"                   # "app" or "none" (404)
trust_forwarded_headers = false    # keep client X-Forwarded-* values
connect_timeout_ms = 5000
read_timeout_ms = 60000            # 0: off
write_timeout_ms = 60000           # 0: off
max_retries = 1                    # connect retries on another upstream
health_check_interval_ms = 5000    # 0: off
health_check_timeout_ms = 1000
max_request_body_bytes = 10485760  # 0: off
max_connections = 10000            # 0: off
shutdown_grace_ms = 10000
metrics = true

[[pingora.routes]]
name = "billing"                   # unique; used in metrics
host = ""                          # "" (any), "api.example.com" or "*.example.com"
path_prefix = "/billing"
upstreams = ["10.0.0.7:8080", "billing.internal:8080"]
strip_prefix = true
upstream_host = ""                 # "": keep the client Host
selection = "round_robin"          # or "consistent"
```

`AUTUMN_PINGORA__ROUTES` replaces all routes with a TOML array.

## Metrics

| Name | Type | Labels |
|---|---|---|
| `pingora_proxy_up` | gauge | — |
| `pingora_proxy_connections_active` | gauge | — |
| `pingora_proxy_upstreams` | gauge | `route` |
| `pingora_proxy_upstreams_healthy` | gauge | `route` |
| `pingora_proxy_requests_total` | counter | `route`, `status` (`2xx`, …, `error`) |
| `pingora_proxy_upstream_errors_total` | counter | `route` |

`route` is a route name, `fallback` or `unmatched`.

## Behind the proxy

With `fallback = "app"`, the app sees the proxy (`127.0.0.1`) as the
client. Tell Autumn to trust it:

```toml
[security.trusted_proxies]
trust_forwarded_headers = true
ranges = ["127.0.0.1/32"]
```

The plugin warns at boot when this is missing.

## Limits of v0.1

- No TLS. Put the proxy behind a TLS terminator, or on a private network.
  `fallback = "app"` with `[server.tls]` stops boot.
- No cache, rate limits or custom filters.
- Hostnames resolve once, at boot.
- Linux and macOS only (Pingora).

## Design

See `docs/plan.md` (brainstorming, reverse brainstorming, six hats, AC)
and `docs/adr/`.

## License

Apache-2.0.
