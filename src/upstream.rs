//! Upstream pools: selection and TCP health checks.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::time::Duration;

use pingora_load_balancing::LoadBalancer;
use pingora_load_balancing::health_check::TcpHealthCheck;
use pingora_load_balancing::selection::{Consistent, RoundRobin};

use crate::config::{Route, Selection};
use crate::error::PingoraError;

/// Most candidates a selection looks at.
const MAX_ITERATIONS: usize = 256;

enum Balancer {
    RoundRobin(LoadBalancer<RoundRobin>),
    Consistent(LoadBalancer<Consistent>),
}

/// Run `$body` with `$lb` bound to the inner `LoadBalancer`.
macro_rules! with_lb {
    ($balancer:expr, $lb:ident => $body:expr) => {
        match $balancer {
            Balancer::RoundRobin($lb) => $body,
            Balancer::Consistent($lb) => $body,
        }
    };
}

/// The upstreams of one route.
pub struct Pool {
    balancer: Balancer,
    total: usize,
}

impl std::fmt::Debug for Pool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pool")
            .field("total", &self.total)
            .finish_non_exhaustive()
    }
}

impl Pool {
    /// Resolve the upstreams of `route` and build its pool. The health
    /// check uses `check_timeout` to connect.
    ///
    /// # Errors
    ///
    /// [`PingoraError::Resolve`] when an upstream does not resolve.
    pub async fn build(route: &Route, check_timeout: Duration) -> Result<Self, PingoraError> {
        let mut addrs = BTreeSet::new();
        for upstream in &route.upstreams {
            let resolved: Vec<SocketAddr> = tokio::net::lookup_host(upstream.as_str())
                .await
                .map_err(|e| resolve_error(route, upstream, &e.to_string()))?
                .collect();
            if resolved.is_empty() {
                return Err(resolve_error(route, upstream, "no address"));
            }
            addrs.extend(resolved);
        }
        let total = addrs.len();
        let mut check = TcpHealthCheck::new();
        check.peer_template.options.connection_timeout = Some(check_timeout);
        let balancer = match route.selection {
            Selection::RoundRobin => {
                let mut lb = LoadBalancer::<RoundRobin>::try_from_iter(addrs)
                    .map_err(|e| resolve_error(route, "*", &e.to_string()))?;
                lb.set_health_check(check);
                Balancer::RoundRobin(lb)
            }
            Selection::Consistent => {
                let mut lb = LoadBalancer::<Consistent>::try_from_iter(addrs)
                    .map_err(|e| resolve_error(route, "*", &e.to_string()))?;
                lb.set_health_check(check);
                Balancer::Consistent(lb)
            }
        };
        Ok(Self { balancer, total })
    }

    /// A healthy upstream that is not in `skip`. `key` feeds the hash.
    pub fn select(&self, key: &[u8], skip: &[SocketAddr]) -> Option<SocketAddr> {
        let accept = |backend: &pingora_load_balancing::Backend, healthy: bool| {
            healthy
                && backend
                    .addr
                    .as_inet()
                    .is_some_and(|addr| !skip.contains(addr))
        };
        let backend = with_lb!(&self.balancer, lb => lb.select_with(key, MAX_ITERATIONS, accept))?;
        backend.addr.as_inet().copied()
    }

    /// Healthy upstreams.
    pub fn healthy(&self) -> usize {
        let backends = with_lb!(&self.balancer, lb => lb.backends());
        backends
            .get_backend()
            .iter()
            .filter(|b| backends.ready(b))
            .count()
    }

    /// All upstreams, after resolution.
    pub const fn total(&self) -> usize {
        self.total
    }

    /// Run one health check round on all upstreams.
    pub async fn check(&self) {
        with_lb!(&self.balancer, lb => lb.backends().run_health_check(true).await);
    }
}

fn resolve_error(route: &Route, upstream: &str, reason: &str) -> PingoraError {
    PingoraError::Resolve {
        route: route.name.clone(),
        upstream: upstream.to_owned(),
        reason: reason.to_owned(),
    }
}
