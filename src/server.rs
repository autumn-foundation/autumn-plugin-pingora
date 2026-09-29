//! The accept loop, the drain and [`PingoraHandle`] (ADR 0001, ADR 0003).

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use pingora_core::apps::{HttpServerApp, ServerApp};
use pingora_core::protocols::GetSocketDigest;
use pingora_core::protocols::l4::listener::Listener;
use pingora_core::protocols::l4::stream::Stream;
use pingora_proxy::HttpProxy;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::error::PingoraError;
use crate::lifecycle::{Lifecycle, LifecycleCell, LifecycleEvent};
use crate::metrics::Metrics;
use crate::proxy::Gateway;
use crate::upstream::Pool;

/// Wait after an `accept` error. Then a full fd table does not use all
/// the CPU.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// Upstream health for one route.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct UpstreamHealth {
    /// The route name.
    pub route: String,
    /// Upstreams that passed the last health check.
    pub healthy: usize,
    /// All upstreams, after name resolution. One entry per address.
    pub total: usize,
}

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

    /// Upstream health for each route, in route order.
    pub fn upstream_health(&self) -> Vec<UpstreamHealth> {
        self.pools.get().map_or_else(Vec::new, |(names, pools)| {
            names
                .iter()
                .zip(pools.iter())
                .map(|(name, pool)| UpstreamHealth {
                    route: name.clone(),
                    healthy: pool.healthy(),
                    total: pool.total(),
                })
                .collect()
        })
    }

    /// Record a start failure. Waiters on `shutdown` return. A proxy that
    /// already serves does not change.
    pub fn fail(&self) {
        if self.lifecycle.apply(LifecycleEvent::StartFailed).is_ok() {
            self.done.send_replace(true);
        }
    }

    /// Apply `event`. Wake the waiters when the state is terminal.
    fn finish(&self, event: LifecycleEvent) {
        let _ = self.lifecycle.apply(event);
        if self.lifecycle.get().is_terminal() {
            self.done.send_replace(true);
        }
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
    pub max_connections_per_ip: usize,
    pub grace: Duration,
    pub health_interval: Option<Duration>,
}

/// The result of [`start`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Started {
    /// The proxy serves on this address.
    Serving(SocketAddr),
    /// Shutdown came first. Nothing serves.
    Stopped,
}

/// Start serving. The caller has bound the listener.
///
/// # Errors
///
/// [`PingoraError::AlreadyStarted`] when another start won. An I/O error
/// when the listener has no local address.
pub fn start(shared: &Arc<Shared>, launch: Launch) -> Result<Started, PingoraError> {
    let addr = launch
        .listener
        .local_addr()
        .map_err(|source| PingoraError::Bind {
            addr: SocketAddr::from(([0, 0, 0, 0], 0)),
            source,
        })?;
    match shared.lifecycle.apply(LifecycleEvent::Bound) {
        Ok(_) => {}
        Err(Lifecycle::Stopped) => return Ok(Started::Stopped),
        Err(_) => return Err(PingoraError::AlreadyStarted),
    }
    let _ = shared.local_addr.set(addr);
    let _ = shared.pools.set((launch.route_names, launch.pools.clone()));
    if let Some(interval) = launch.health_interval {
        tokio::spawn(check_health(launch.pools, interval, shared.stop.clone()));
    }
    let limit =
        (launch.max_connections > 0).then(|| Arc::new(Semaphore::new(launch.max_connections)));
    let proxy = Arc::new(launch.proxy);
    let per_ip = PerIp::new(launch.max_connections_per_ip);
    // The guard lives in the future, so it drops even when the task never
    // runs.
    let exit = ExitGuard(Some(shared.clone()));
    tokio::spawn(serve(
        exit,
        Listener::from(launch.listener),
        proxy,
        Limits {
            total: limit,
            per_ip,
        },
        launch.grace,
    ));
    Ok(Started::Serving(addr))
}

/// Health check rounds until shutdown. The first round runs at once.
async fn check_health(pools: Arc<Vec<Pool>>, interval: Duration, stop: CancellationToken) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = stop.cancelled() => return,
            _ = ticker.tick() => {
                futures_util::future::join_all(pools.iter().map(Pool::check)).await;
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

/// Open connections per client IP.
#[derive(Debug)]
struct PerIp {
    limit: usize,
    open: Mutex<HashMap<IpAddr, usize>>,
}

impl PerIp {
    fn new(limit: usize) -> Option<Arc<Self>> {
        (limit > 0).then(|| {
            Arc::new(Self {
                limit,
                open: Mutex::new(HashMap::new()),
            })
        })
    }

    /// A slot for `ip`, or `None` at the limit.
    fn take(self: &Arc<Self>, ip: IpAddr) -> Option<IpSlot> {
        let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        let count = open.entry(ip).or_insert(0);
        let admitted = *count < self.limit;
        if admitted {
            *count += 1;
        }
        drop(open);
        admitted.then(|| IpSlot(self.clone(), ip))
    }
}

/// Gives back one per-IP slot on drop.
struct IpSlot(Arc<PerIp>, IpAddr);

impl Drop for IpSlot {
    fn drop(&mut self) {
        let mut open = self.0.open.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = open.get_mut(&self.1) {
            *count -= 1;
            if *count == 0 {
                open.remove(&self.1);
            }
        }
    }
}

/// Connection limits.
struct Limits {
    total: Option<Arc<Semaphore>>,
    per_ip: Option<Arc<PerIp>>,
}

/// The per-IP slot for `stream`. `Err` when the client is at its limit.
fn ip_slot(per_ip: Option<&Arc<PerIp>>, stream: &Stream) -> Result<Option<IpSlot>, ()> {
    let Some(per_ip) = per_ip else {
        return Ok(None);
    };
    let ip = stream.get_socket_digest().and_then(|digest| {
        digest
            .peer_addr()
            .and_then(|a| a.as_inet())
            .map(SocketAddr::ip)
    });
    ip.map_or(Ok(None), |ip| per_ip.take(ip).map(Some).ok_or(()))
}

/// Accept until shutdown, then drain.
async fn serve(
    mut exit: ExitGuard,
    listener: Listener,
    proxy: Arc<HttpProxy<Gateway>>,
    limits: Limits,
    grace: Duration,
) {
    let Some(shared) = exit.0.clone() else {
        return;
    };
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut connections = JoinSet::new();
    loop {
        let Some(permit) = slot(limits.total.as_ref(), &shared.stop).await else {
            break;
        };
        tokio::select! {
            () = shared.stop.cancelled() => break,
            accepted = listener.accept() => match accepted {
                Ok(stream) => {
                    let Ok(ip) = ip_slot(limits.per_ip.as_ref(), &stream) else {
                        tracing::debug!("pingora proxy: client at max_connections_per_ip");
                        continue;
                    };
                    let active = Active::new(&shared);
                    let proxy = proxy.clone();
                    let shutdown = shutdown_rx.clone();
                    connections.spawn(async move {
                        serve_connection(proxy, stream, shutdown).await;
                        drop((active, permit, ip));
                    });
                }
                Err(error) => {
                    tracing::warn!(%error, "pingora proxy: accept failed");
                    tokio::select! {
                        () = shared.stop.cancelled() => {}
                        () = tokio::time::sleep(ACCEPT_BACKOFF) => {}
                    }
                }
            },
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    // Close the listener first: new connections get "refused".
    drop(listener);
    drain(&proxy, &shutdown_tx, &mut connections, grace).await;
    exit.0 = None;
    shared.finish(LifecycleEvent::Drained);
}

/// Records `ServerExited` when the serve task ends early, for example when
/// its runtime stops. Then `shutdown` does not wait forever.
struct ExitGuard(Option<Arc<Shared>>);

impl Drop for ExitGuard {
    fn drop(&mut self) {
        if let Some(shared) = self.0.take() {
            shared.stop.cancel();
            shared.finish(LifecycleEvent::ServerExited);
        }
    }
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
            "pingora proxy: the grace period ended; the proxy closes the open connections"
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

    /// Upstream health for each route, in route order.
    #[must_use]
    pub fn upstream_health(&self) -> Vec<UpstreamHealth> {
        self.shared.upstream_health()
    }

    /// The metric families, as `/actuator/prometheus` shows them.
    #[must_use]
    pub fn metric_families(&self) -> Vec<autumn_web::actuator::MetricFamily> {
        crate::metrics::families(&self.shared)
    }

    /// Stop the proxy and wait for the drain. You can call it more than one
    /// time, also before boot. The drain runs in its own task. It continues
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
