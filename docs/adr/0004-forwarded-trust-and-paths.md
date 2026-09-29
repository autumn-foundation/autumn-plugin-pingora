# ADR 0004 — Forwarded-header trust and path safety

Status: accepted. Came from the security review.

## Context

A reverse proxy decides what the upstream believes about the client. A
client can send `X-Forwarded-For`, `X-Real-IP`, `X-Forwarded-Prefix` and
other headers. Some servers trust them. A single switch that trusts all
peers lets any internet client set them.

Upstream servers also normalize paths in different ways. Tomcat removes
`;params`. Some servers decode `%2F` or accept `\` as a separator. So
`/api/..;/admin` can pass a `/api` prefix check and reach `/admin`.

## Decision

- `trusted_proxies` is a list of peer IPs or CIDR ranges. The proxy keeps
  client identity headers only when the socket peer is in the list.
- For other peers, the proxy sets `X-Forwarded-For/Proto/Host`. It
  removes `Forwarded`, `X-Real-IP`, `True-Client-IP`, `X-Client-IP`,
  `CF-Connecting-IP`, `X-Cluster-Client-IP`, `X-Original-Forwarded-For`
  and all other `X-Forwarded-*` headers.
- A path is unsafe when, after two percent-decodes and with `;params`
  removed, a segment is `.` or `..`, or when it has a backslash or a NUL.
  Unsafe paths get 400. An encoded slash alone is safe.

## Consequences

- Behind a load balancer, list its addresses in `trusted_proxies`.
- Some legal but odd paths get 400. Normalizing the path in the proxy is a
  possible follow-up.
