# ADR 0002 — Fallback to the Autumn app

Status: accepted.

## Context

Most users put the proxy in front of their Autumn app and move a few
prefixes to other services. An unmatched request must not surprise them.

## Decision

`fallback = "app"` is the default. The plugin reads `server.host` and
`server.port` from Autumn's config at startup. An unspecified host
(`0.0.0.0`, `::`) becomes loopback. `fallback = "none"` returns 404.

Boot stops when:

- Autumn serves TLS (`[server.tls]`). v0.1 has no upstream TLS.
- The proxy address is the app address. Requests would loop.

Boot warns when `[security.trusted_proxies]` does not trust loopback.
Without trust, the app sees the proxy as the client.

## Consequences

- Zero routes give an in-process edge proxy.
- The fallback is not a pool. It has no health check. Autumn's own
  health covers it.
