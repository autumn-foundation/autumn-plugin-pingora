//! The Pingora `ProxyHttp` implementation: route, pick, rewrite, count.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_error::{Error, ErrorSource, ErrorType, Result};
use pingora_http::RequestHeader;
use pingora_proxy::{FailToProxy, ProxyHttp, Session};

use crate::config::Route;
use crate::metrics::{FALLBACK, Metrics, UNMATCHED};
use crate::router::{Router, has_dot_segment, strip_prefix};
use crate::upstream::Pool;

/// Where a request goes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Target {
    /// No decision yet, or no route and no fallback.
    #[default]
    Unmatched,
    /// The route at this index.
    Route(usize),
    /// The Autumn app.
    Fallback,
}

/// Per-request state.
#[derive(Debug, Default)]
pub struct Ctx {
    target: Target,
    /// The client `Host` (or `:authority`).
    host: Option<String>,
    /// Request body bytes so far.
    body_bytes: u64,
    /// Upstreams that failed to connect for this request.
    tried: Vec<SocketAddr>,
    /// The status sent by the proxy itself, when it sent one.
    status: u16,
}

/// Timeouts for upstream connections.
#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    pub connect: Duration,
    pub read: Option<Duration>,
    pub write: Option<Duration>,
}

/// The proxy logic. One instance serves all connections.
pub struct Gateway {
    pub routes: Vec<Route>,
    pub pools: Arc<Vec<Pool>>,
    pub router: Router,
    pub fallback: Option<SocketAddr>,
    pub trust_forwarded: bool,
    pub max_body: u64,
    pub timeouts: Timeouts,
    pub metrics: Option<Arc<Metrics>>,
}

impl Gateway {
    fn label(&self, target: Target) -> &str {
        match target {
            Target::Route(index) => self
                .routes
                .get(index)
                .map_or(UNMATCHED, |r| r.name.as_str()),
            Target::Fallback => FALLBACK,
            Target::Unmatched => UNMATCHED,
        }
    }

    fn count_upstream_error(&self, ctx: &Ctx) {
        if let Some(metrics) = &self.metrics {
            metrics.upstream_error(self.label(ctx.target));
        }
    }

    fn peer(&self, addr: SocketAddr) -> Box<HttpPeer> {
        let mut peer = HttpPeer::new(addr, false, String::new());
        peer.options.connection_timeout = Some(self.timeouts.connect);
        peer.options.read_timeout = self.timeouts.read;
        peer.options.write_timeout = self.timeouts.write;
        Box::new(peer)
    }
}

/// The client IP, from the socket.
fn client_ip(session: &Session) -> Option<IpAddr> {
    session
        .client_addr()
        .and_then(|addr| addr.as_inet())
        .map(SocketAddr::ip)
}

/// The `Host` header, or the URI authority (HTTP/2).
fn request_host(header: &RequestHeader) -> Option<String> {
    header
        .headers
        .get(http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .or_else(|| header.uri.authority().map(|a| a.as_str().to_owned()))
}

/// Send `status` with no body and end the connection.
async fn reject(session: &mut Session, ctx: &mut Ctx, status: u16) -> Result<bool> {
    ctx.status = status;
    session.set_keepalive(None);
    session.respond_error(status).await?;
    Ok(true)
}

/// The status for a failed request. Upstream timeouts give 504.
pub fn error_status(error: &Error) -> u16 {
    use ErrorType::{
        ConnectTimedout, ConnectionClosed, HTTPStatus, ReadError, ReadTimedout, WriteError,
        WriteTimedout,
    };
    match (error.etype(), error.esource()) {
        (HTTPStatus(code), _) => *code,
        (ConnectTimedout | ReadTimedout | WriteTimedout, ErrorSource::Upstream) => 504,
        (_, ErrorSource::Upstream) => 502,
        (WriteError | ReadError | ConnectionClosed, ErrorSource::Downstream) => 0,
        (_, ErrorSource::Downstream) => 400,
        (_, ErrorSource::Internal | ErrorSource::Unset) => 500,
    }
}

#[async_trait]
impl ProxyHttp for Gateway {
    type CTX = Ctx;

    fn new_ctx(&self) -> Ctx {
        Ctx::default()
    }

    async fn request_filter(&self, session: &mut Session, ctx: &mut Ctx) -> Result<bool> {
        let header = session.req_header();
        if has_dot_segment(header.uri.path()) {
            return reject(session, ctx, 400).await;
        }
        ctx.host = request_host(header);
        ctx.target = match self.router.find(ctx.host.as_deref(), header.uri.path()) {
            Some(index) => Target::Route(index),
            None if self.fallback.is_some() => Target::Fallback,
            None => return reject(session, ctx, 404).await,
        };
        let declared = header
            .headers
            .get(http::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        if self.max_body > 0 && declared.is_some_and(|len| len > self.max_body) {
            return reject(session, ctx, 413).await;
        }
        Ok(false)
    }

    async fn upstream_peer(&self, session: &mut Session, ctx: &mut Ctx) -> Result<Box<HttpPeer>> {
        match ctx.target {
            Target::Route(index) => {
                let key = client_ip(session)
                    .map(|ip| ip.to_string())
                    .unwrap_or_default();
                let pick = self
                    .pools
                    .get(index)
                    .and_then(|pool| pool.select(key.as_bytes(), &ctx.tried));
                pick.map_or_else(
                    || {
                        self.count_upstream_error(ctx);
                        Err(Error::explain(
                            ErrorType::HTTPStatus(503),
                            "no healthy upstream",
                        ))
                    },
                    |addr| Ok(self.peer(addr)),
                )
            }
            Target::Fallback => self.fallback.map_or_else(
                || Err(Error::explain(ErrorType::HTTPStatus(404), "no fallback")),
                |addr| Ok(self.peer(addr)),
            ),
            Target::Unmatched => Err(Error::explain(ErrorType::HTTPStatus(404), "no route")),
        }
    }

    async fn request_body_filter(
        &self,
        _session: &mut Session,
        body: &mut Option<Bytes>,
        _end_of_stream: bool,
        ctx: &mut Ctx,
    ) -> Result<()> {
        if let Some(chunk) = body {
            ctx.body_bytes = ctx.body_bytes.saturating_add(chunk.len() as u64);
        }
        if self.max_body > 0 && ctx.body_bytes > self.max_body {
            return Err(Error::explain(
                ErrorType::HTTPStatus(413),
                "request body too large",
            ));
        }
        Ok(())
    }

    async fn upstream_request_filter(
        &self,
        session: &mut Session,
        upstream: &mut RequestHeader,
        ctx: &mut Ctx,
    ) -> Result<()> {
        let route = match ctx.target {
            Target::Route(index) => self.routes.get(index),
            _ => None,
        };
        if let Some(route) = route.filter(|r| r.strip_prefix) {
            let target = upstream
                .uri
                .path_and_query()
                .map_or("/", http::uri::PathAndQuery::as_str);
            let stripped = strip_prefix(target, &route.path_prefix);
            let uri = stripped
                .parse::<http::Uri>()
                .map_err(|e| Error::because(ErrorType::InternalError, "strip prefix", e))?;
            upstream.set_uri(uri);
        }
        let changes = crate::forwarded::changes(
            client_ip(session),
            &session.req_header().headers,
            ctx.host.as_deref(),
            self.trust_forwarded,
        );
        for (name, value) in changes {
            match value {
                Some(value) => upstream.insert_header(name, value)?,
                None => {
                    upstream.remove_header(name);
                }
            }
        }
        match route
            .map(|r| r.upstream_host.as_str())
            .filter(|h| !h.is_empty())
        {
            Some(host) => upstream.insert_header(http::header::HOST, host)?,
            None => {
                if upstream.headers.get(http::header::HOST).is_none()
                    && let Some(host) = &ctx.host
                {
                    upstream.insert_header(http::header::HOST, host.as_str())?;
                }
            }
        }
        Ok(())
    }

    fn fail_to_connect(
        &self,
        _session: &mut Session,
        peer: &HttpPeer,
        ctx: &mut Ctx,
        mut e: Box<Error>,
    ) -> Box<Error> {
        self.count_upstream_error(ctx);
        if let Some(addr) = peer._address.as_inet() {
            ctx.tried.push(*addr);
        }
        // No byte reached the upstream, so a retry is safe. Pingora caps
        // the count with `max_retries`.
        e.set_retry(true);
        e
    }

    fn error_while_proxy(
        &self,
        peer: &HttpPeer,
        session: &mut Session,
        e: Box<Error>,
        ctx: &mut Ctx,
        client_reused: bool,
    ) -> Box<Error> {
        if e.esource() == &ErrorSource::Upstream {
            self.count_upstream_error(ctx);
        }
        let mut e = e.more_context(format!("peer: {peer}"));
        if !session.req_header().method.is_idempotent() || session.as_ref().retry_buffer_truncated()
        {
            e.set_retry(false);
        } else {
            e.retry.decide_reuse(client_reused);
        }
        e
    }

    async fn fail_to_proxy(&self, session: &mut Session, e: &Error, ctx: &mut Ctx) -> FailToProxy {
        let code = error_status(e);
        if code > 0 {
            ctx.status = code;
            if let Err(error) = session.respond_error(code).await {
                tracing::debug!(%error, "cannot send the error response");
            }
        }
        FailToProxy {
            error_code: code,
            can_reuse_downstream: false,
        }
    }

    async fn logging(&self, session: &mut Session, _e: Option<&Error>, ctx: &mut Ctx) {
        let Some(metrics) = &self.metrics else {
            return;
        };
        let status = session
            .response_written()
            .map_or(ctx.status, |response| response.status.as_u16());
        metrics.record(self.label(ctx.target), status);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_timeouts_give_504_and_other_upstream_errors_502() {
        let timeout = Error::new(ErrorType::ReadTimedout).into_up();
        assert_eq!(error_status(&timeout), 504);
        let refused = Error::new(ErrorType::ConnectRefused).into_up();
        assert_eq!(error_status(&refused), 502);
        let client_gone = Error::new(ErrorType::ConnectionClosed).into_down();
        assert_eq!(error_status(&client_gone), 0);
        let client_timeout = Error::new(ErrorType::ReadTimedout).into_down();
        assert_eq!(error_status(&client_timeout), 400);
        let status = Error::new(ErrorType::HTTPStatus(413));
        assert_eq!(error_status(&status), 413);
        assert_eq!(error_status(&Error::new(ErrorType::InternalError)), 500);
    }
}
