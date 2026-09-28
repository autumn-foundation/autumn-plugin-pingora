# CLAUDE.md — autumn-plugin-pingora

Pingora reverse proxy plugin for Autumn 0.7, on Pingora 0.9. One crate.

## Commands

- Format: `cargo fmt --all` (CI: `--check`)
- Lint: `cargo clippy --all-targets -- -D warnings` (pedantic + nursery)
- Test: `cargo test`
- Docs: `RUSTDOCFLAGS=-D warnings cargo doc --no-deps`
- Coverage: `cargo llvm-cov --fail-under-lines 85`
- Example: `cargo run --example gateway`
- Pre-commit hook: `git config core.hooksPath .githooks`

## Layout

`plugin.rs` (builder, `Plugin`, launch, fallback checks), `config.rs`,
`server.rs` (accept loop, drain, `PingoraHandle`), `proxy.rs`
(`ProxyHttp`), `upstream.rs` (pools, health checks), `router.rs` and
`forwarded.rs` (pure), `lifecycle.rs`, `metrics.rs`, `health.rs`,
`error.rs`.

## Rules

- **Do not use Pingora's `Server` or listening `Service`** (ADR 0001).
  Autumn owns the process and the runtime.
- **State changes only through `LifecycleCell::apply`.** Change the spec
  table in `tests/lifecycle.rs` first.
- **Drain work runs in a spawned task**, not in the caller (ADR 0003).
- **Pingora `max_retries` counts attempts.** Set it to retries + 1.
- **Metric labels stay bounded:** route names, `fallback`, `unmatched`,
  status classes. Names start with `pingora_proxy_`, never `autumn_`.
- **Config:** a new key goes in `config.rs` with a safe default, a doc
  comment, validation, and a line in the README TOML block. Use `0`/`""`
  for "unset".
- **Startup errors abort boot.** Return `PingoraError`. Never fall back
  silently.
- **Tests:** pin `.development(..)`. Wait with `common::eventually`, not
  a fixed sleep. Tests use real upstreams on port 0.
- No `unwrap`/`expect`/`panic!` in library code.
- No behavior without a test. Work RED → GREEN → REFACTOR.
- Docs and comments: short, ASD-STE100 (simple words, active voice).

## Autumn API notes (0.7.0 on crates.io)

- `TestApp` runs startup hooks, not shutdown hooks. Tests call
  `PingoraHandle::shutdown`. A failed hook panics in `build()`.
- `Plugin::contract` does not exist in 0.7.0.
- `AppState::shutdown_token` needs the `ws` feature.
- Health details show only with `[health] detailed = true`.
