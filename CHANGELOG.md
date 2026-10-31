# Changelog

## 0.2.0 — 2026-10-31

### Changed

- **Breaking:** Support `autumn-web` 0.9. The plugin now needs `autumn-web`
  0.9. Apps on `autumn-web` 0.8 stay on 0.1.

## 0.1.0 — 2026-10-05

First release.

- `PingoraPlugin`: a Pingora reverse proxy on its own listener.
- Routes by host and path prefix, with prefix strip and `Host` rewrite.
- Fallback to the Autumn app, or 404.
- Round-robin and consistent-hash pools, TCP health checks, retries.
- `X-Forwarded-*` headers with a `trusted_proxies` peer list, and removal
  of other client identity headers.
- 400 for paths with dot segments (also encoded) or backslashes.
- Body, connection and per-IP connection limits.
- `pingora` health indicator, `pingora_proxy_*` metrics.
- Graceful drain on Autumn's shutdown signal.
- `[pingora]` config with profiles and env overrides.
