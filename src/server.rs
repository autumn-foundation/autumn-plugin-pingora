//! The accept loop, the drain and [`PingoraHandle`] (ADR 0001, ADR 0003).

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use pingora_core::apps::{HttpServerApp, ServerApp};
use pingora_core::protocols::l4::listener::Listener;
use pingora_core::protocols::l4::stream::Stream;
use pingora_proxy::HttpProxy;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::lifecycle::{Lifecycle, LifecycleCell, LifecycleEvent};
use crate::metrics::Metrics;
use crate::proxy::Gateway;
use crate::upstream::Pool;

/// Pause after an `accept` error, so a full fd table does not spin.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// State shared by the plugin, the server and the handles.
#[derive(Debug)]
pub struct Shared {
    pub lifecycle: LifecycleCell,
    pub local_addr: OnceLock<SocketAddr>,
    pub metrics: OnceLock<Arc<Metrics>>,
    pools: OnceLock<(Vec<String>, Arc<Vec<Pool>>)>,
    active: AtomicUsize,
    /// Fires when shutdown starts.
    stop: CancellationToken,
    /// `true` when the proxy reached a terminal state.
    done: watch::Sender<bool>,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            lifecycle: LifecycleCell::new(),
            local_addr: OnceLock::new(),
            metrics: OnceLock::new(),
            pools: OnceLock::new(),
            active: AtomicUsize::new(0),
            stop: CancellationToken::new(),
            done: watch::channel(false).0,
        }
    }
}

impl Shared {
    /// Open client connections.
    pub fn active_connections(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }

    /// `(route, healthy, total)` for each route.
    pub fn upstream_health(&self) -> Vec<(String, usize, usize)> {
        self.pools.get().map_or_else(Vec::new, |(names, pools)| {
            names
                .iter()
                .zip(pools.iter())
                .map(|(name, pool)| (name.clone(), pool.healthy(), pool.total()))
                .collect()
        })
    }

    /// Record a start failure. Waiters on `shutdown` return.
    pub fn fail(&self) {
        let _ = self.lifecycle.apply(LifecycleEvent::StartFailed);
        self.done.send_replace(true);
    }

    fn finish(&self, event: LifecycleEvent) {
        let _ = self.lifecycle.apply(event);
        self.done.send_replace(true);
    }
}

/// Counts one open connection while it lives.
struct Active(Arc<Shared>);

impl Active {
    fn new(shared: &Arc<Shared>) -> Self {
        shared.active.fetch_add(1, Ordering::AcqRel);
        Self(shared.clone())
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}

/// What the accept loop needs.
pub struct Launch {
    pub listener: tokio::net::TcpListener,
    pub proxy: HttpProxy<Gateway>,
    pub pools: Arc<Vec<Pool>>,
    pub route_names: Vec<String>,
    pub max_connections: usize,
    pub grace: Duration,
    pub health_interval: Option<Duration>,
}

/// Start serving. The caller has bound the listener.
///
/// Returns the bound address.
///
/// # Errors
///
/// Returns the I/O error when the listener has no local address.
pub fn start(shared: &Arc<Shared>, launch: Launch) -> std::io::Result<SocketAddr> {
    let addr = launch.listener.local_addr()?;
    let _ = shared.local_addr.set(addr);
    let _ = shared.pools.set((launch.route_names, launch.pools.clone()));
    if shared.lifecycle.apply(LifecycleEvent::Bound).is_err() {
        // Shutdown won the race before the bind finished.
        return Ok(addr);
    }
    if let Some(interval) = launch.health_interval {
        tokio::spawn(check_health(launch.pools, interval, shared.stop.clone()));
    }
    let limit =
        (launch.max_connections > 0).then(|| Arc::new(Semaphore::new(launch.max_connections)));
    let proxy = Arc::new(launch.proxy);
    tokio::spawn(serve(
        shared.clone(),
        Listener::from(launch.listener),
        proxy,
        limit,
        launch.grace,
    ));
    Ok(addr)
}

/// Health check rounds until shutdown. The first round runs at once.
async fn check_health(pools: Arc<Vec<Pool>>, interval: Duration, stop: CancellationToken) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = stop.cancelled() => return,
            _ = ticker.tick() => {
                for pool in pools.iter() {
                    pool.check().await;
                }
            }
        }
    }
}

/// Wait for a connection slot. `None` when shutdown starts first.
async fn slot(
    limit: Option<&Arc<Semaphore>>,
    stop: &CancellationToken,
) -> Option<Option<OwnedSemaphorePermit>> {
    let Some(limit) = limit else {
        return Some(None);
    };
    tokio::select! {
        () = stop.cancelled() => None,
        permit = limit.clone().acquire_owned() => permit.ok().map(Some),
    }
}

/// Accept until shutdown, then drain.
async fn serve(
    shared: Arc<Shared>,
    listener: Listener,
    proxy: Arc<HttpProxy<Gateway>>,
    limit: Option<Arc<Semaphore>>,
    grace: Duration,
) {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut connections = JoinSet::new();
    loop {
        let Some(permit) = slot(limit.as_ref(), &shared.stop).await else {
            break;
        };
        tokio::select! {
            () = shared.stop.cancelled() => break,
            accepted = listener.accept() => match accepted {
                Ok(stream) => {
                    let active = Active::new(&shared);
                    let proxy = proxy.clone();
                    let shutdown = shutdown_rx.clone();
                    connections.spawn(async move {
                        serve_connection(proxy, stream, shutdown).await;
                        drop((active, permit));
                    });
                }
                Err(error) => {
                    tracing::warn!(%error, "pingora proxy: accept failed");
                    tokio::time::sleep(ACCEPT_BACKOFF).await;
                }
            },
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    // Close the listener first: new connections get "refused".
    drop(listener);
    drain(&proxy, &shutdown_tx, &mut connections, grace).await;
    shared.finish(LifecycleEvent::Drained);
}

/// End keep-alive, wait for open requests up to `grace`, then abort.
async fn drain(
    proxy: &HttpProxy<Gateway>,
    shutdown: &watch::Sender<bool>,
    connections: &mut JoinSet<()>,
    grace: Duration,
) {
    shutdown.send_replace(true);
    proxy.http_cleanup().await;
    let finished = tokio::time::timeout(grace, async {
        while connections.join_next().await.is_some() {}
    })
    .await;
    if finished.is_err() {
        tracing::warn!(
            open = connections.len(),
            "pingora proxy: grace period over; closing open connections"
        );
        connections.abort_all();
        while connections.join_next().await.is_some() {}
    }
}

/// Serve one client connection, with keep-alive reuse.
async fn serve_connection(
    proxy: Arc<HttpProxy<Gateway>>,
    stream: Stream,
    shutdown: watch::Receiver<bool>,
) {
    let mut next: Option<pingora_core::protocols::Stream> = Some(Box::new(stream));
    while let Some(stream) = next {
        next = proxy.process_new(stream, &shutdown).await;
    }
}

/// A handle to the proxy. Clone it freely. It is valid before and after
/// boot.
#[derive(Debug, Clone)]
pub struct PingoraHandle {
    shared: Arc<Shared>,
}

impl PingoraHandle {
    pub(crate) const fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

    /// The current state.
    #[must_use]
    pub fn state(&self) -> Lifecycle {
        self.shared.lifecycle.get()
    }

    /// The bound address, after boot.
    #[must_use]
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.shared.local_addr.get().copied()
    }

    /// Open client connections.
    #[must_use]
    pub fn active_connections(&self) -> usize {
        self.shared.active_connections()
    }

    /// `(route, healthy, total)` upstream counts, in route order.
    #[must_use]
    pub fn upstream_health(&self) -> Vec<(String, usize, usize)> {
        self.shared.upstream_health()
    }

    /// The metric families, as `/actuator/prometheus` shows them.
    #[must_use]
    pub fn metric_families(&self) -> Vec<autumn_web::actuator::MetricFamily> {
        crate::metrics::families(&self.shared)
    }

    /// Stop the proxy and wait for the drain. Safe to call more than once,
    /// and before boot. The drain runs in its own task, so it finishes
    /// when the caller drops this future.
    pub async fn shutdown(&self) {
        match self
            .shared
            .lifecycle
            .apply(LifecycleEvent::ShutdownRequested)
        {
            Ok(Lifecycle::Draining) => self.shared.stop.cancel(),
            Ok(_) => {
                self.shared.stop.cancel();
                self.shared.done.send_replace(true);
            }
            Err(_) => {}
        }
        let mut done = self.shared.done.subscribe();
        let _ = done.wait_for(|finished| *finished).await;
    }
}
