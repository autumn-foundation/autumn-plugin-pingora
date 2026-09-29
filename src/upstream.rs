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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    async fn pool(selection: Selection) -> Pool {
        let route = Route::new("r")
            .upstreams(["127.0.0.1:1001", "127.0.0.1:1002", "127.0.0.1:1003"])
            .selection(selection);
        Pool::build(&route, Duration::from_millis(100))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn consistent_keys_spread_and_stay() {
        let pool = pool(Selection::Consistent).await;
        let mut picks = HashSet::new();
        for client in 0..64 {
            let key = format!("10.0.0.{client}");
            let first = pool.select(key.as_bytes(), &[]).unwrap();
            for _ in 0..3 {
                assert_eq!(pool.select(key.as_bytes(), &[]), Some(first), "{key}");
            }
            picks.insert(first);
        }
        assert_eq!(picks.len(), 3, "keys use all upstreams");
    }

    #[tokio::test]
    async fn select_skips_tried_upstreams() {
        for selection in [Selection::RoundRobin, Selection::Consistent] {
            let pool = pool(selection).await;
            let first = pool.select(b"k", &[]).unwrap();
            let second = pool.select(b"k", &[first]).unwrap();
            assert_ne!(first, second);
            let third = pool.select(b"k", &[first, second]).unwrap();
            assert!(pool.select(b"k", &[first, second, third]).is_none());
            assert_eq!(pool.total(), 3);
            assert_eq!(pool.healthy(), 3, "healthy before the first check");
        }
    }

    #[tokio::test]
    async fn empty_resolution_is_an_error() {
        let route = Route::new("r").upstream("no-such-host.invalid:80");
        let error = Pool::build(&route, Duration::from_millis(100))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("route `r`"), "{error}");
    }
}
