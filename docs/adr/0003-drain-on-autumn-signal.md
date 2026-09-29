# ADR 0003 — Drain on Autumn's shutdown signal

Status: accepted.

## Context

Autumn runs plugin shutdown hooks after the HTTP drain. If the proxy
drains only in the hook, it accepts requests for the app after the app
stopped. Those requests fail with 502. Autumn can also drop a hook
future when its budget ends.

## Decision

- Start the drain when `AppState::shutdown_token` fires (feature `ws` of
  `autumn-web`). The shutdown hook starts it too. Both are idempotent.
- The drain runs in a spawned task, not in the caller.
- Drain steps: stop accepting, set the Pingora shutdown watch, call
  `http_cleanup` to end keep-alive, wait for connections up to
  `shutdown_grace_ms`, then abort the rest.
- Cap `shutdown_grace_ms` to fit `server.shutdown_timeout_secs`.

## Consequences

- The proxy and the app drain at the same time.
- A request that reaches the app after the app closed its listener gets
  502. The window is small.
