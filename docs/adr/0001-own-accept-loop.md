# ADR 0001 — Own accept loop on Autumn's runtime

Status: accepted.

## Context

Pingora's `Server::run_forever` owns the process. It makes its own
runtimes, handles signals and calls `exit`. Its listening `Service`
panics when a bind fails. Autumn must own the process, the runtime and
the shutdown order.

## Decision

Do not use Pingora's `Server` or listening `Service`. Bind a
`tokio::net::TcpListener` in the Autumn startup hook. Accept on Autumn's
tokio runtime. Give each connection to `pingora_proxy::http_proxy` as a
`pingora_core` `Stream`. Track each connection task in a `JoinSet`.

## Consequences

- A bind error aborts boot. Port `0` works.
- The plugin controls the connection limit and the drain.
- Proxy work shares Autumn's runtime threads. Pingora's no-steal runtime
  is not used.
- Hot restart (fd hand-off) is not available.
