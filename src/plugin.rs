//! [`PingoraPlugin`]: the builder and the Autumn `Plugin` implementation.

use std::borrow::Cow;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, OnceLock};

use autumn_web::AppState;
use autumn_web::app::AppBuilder;
use autumn_web::config::AutumnConfig;
use autumn_web::plugin::Plugin;
use autumn_web::route_listing::{RouteClassification, RouteInfo};
use pingora_core::server::configuration::ServerConf;

use crate::config::{
    ConfigError, DEFAULT_SECTION, Fallback, PingoraConfig, Resolved, Route, normalize_prefix,
};
use crate::error::PingoraError;
use crate::health::ProxyHealth;
use crate::metrics::{Metrics, ProxyMetrics};
use crate::proxy::{Gateway, Timeouts};
use crate::router::Router;
use crate::server::{Launch, PingoraHandle, Shared};
use crate::upstream::Pool;

/// The name in `Plugin::name`, logs and errors.
pub const PLUGIN_NAME: &str = "autumn-plugin-pingora";

/// The `autumn-web` series this release is tested with.
pub const SUPPORTED_AUTUMN_WEB: &str = "0.7";

/// Time to close aborted connections, inside Autumn's shutdown budget.
const KILL_WAIT_MS: u64 = 500;

type Override = Arc<dyn Fn(&mut PingoraConfig) + Send + Sync>;

/// Runs a Pingora reverse proxy inside an Autumn app.
///
/// ```rust,ignore
/// use autumn_plugin_pingora::{PingoraPlugin, Route};
///
/// autumn_web::app()
///     .routes(routes![index])
///     .plugin(PingoraPlugin::new().route(
///         Route::new("billing").path_prefix("/billing").upstream("10.0.0.7:8080"),
///     ))
///     .run()
///     .await;
/// ```
///
/// The plugin reads `[pingora]` from `autumn.toml` (see [`PingoraConfig`]).
/// Fluent setters apply on top of the file values.
///
/// On boot, the plugin resolves the upstreams, binds the listener (a
/// failure aborts boot), starts health checks, adds a `pingora` health
/// indicator and `pingora_proxy_*` metrics, and puts a [`PingoraHandle`]
/// into `AppState`. It drains on Autumn's shutdown signal.
pub struct PingoraPlugin {
    explicit: Option<Box<PingoraConfig>>,
    overrides: Vec<Override>,
    development: Option<bool>,
    public: bool,
    resolved: OnceLock<Result<Resolved, ConfigError>>,
    shared: Arc<Shared>,
}

impl Default for PingoraPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for PingoraPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PingoraPlugin")
            .field("config", &self.effective_config())
            .finish_non_exhaustive()
    }
}

impl PingoraPlugin {
    /// A plugin with no routes, configured from `[pingora]`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            explicit: None,
            overrides: Vec::new(),
            development: None,
            public: false,
            resolved: OnceLock::new(),
            shared: Arc::new(Shared::default()),
        }
    }

    /// Add a route after the routes from config.
    #[must_use]
    pub fn route(self, route: Route) -> Self {
        self.configure(move |c| c.routes.push(route.clone()))
    }

    /// Change the configuration in code, after files and environment.
    #[must_use]
    pub fn configure(mut self, apply: impl Fn(&mut PingoraConfig) + Send + Sync + 'static) -> Self {
        self.overrides.push(Arc::new(apply));
        self.resolved = OnceLock::new();
        self
    }

    /// Use `config` and read no files or environment variables. Fluent
    /// setters still apply on top.
    #[must_use]
    pub fn config(mut self, config: PingoraConfig) -> Self {
        self.explicit = Some(Box::new(config));
        self.resolved = OnceLock::new();
        self
    }

    /// Force development (`true`) or production (`false`) defaults. By
    /// default, the active profile decides.
    #[must_use]
    pub fn development(mut self, development: bool) -> Self {
        self.development = Some(development);
        self.resolved = OnceLock::new();
        self
    }

    /// Set the listen address, `IP:port`.
    #[must_use]
    pub fn bind(self, addr: impl Into<String>) -> Self {
        let addr = addr.into();
        self.configure(move |c| c.bind.clone_from(&addr))
    }

    /// Set where unmatched requests go.
    #[must_use]
    pub fn fallback(self, fallback: Fallback) -> Self {
        self.configure(move |c| c.fallback = fallback)
    }

    /// Declare the proxied prefixes open to all clients. `autumn routes`
    /// then shows them as public. Without this call they are
    /// unclassified, and `autumn routes audit` fails on them.
    #[must_use]
    pub const fn public(mut self) -> Self {
        self.public = true;
        self
    }

    /// A handle to this plugin's proxy. It is valid before and after boot.
    #[must_use]
    pub fn handle(&self) -> PingoraHandle {
        PingoraHandle::new(self.shared.clone())
    }

    /// The proxied prefixes as `autumn routes` lists them: method `PROXY`,
    /// path `[host]/prefix/*`. The fallback is not listed: the app routes
    /// cover it.
    #[must_use]
    pub fn route_infos(&self) -> Vec<RouteInfo> {
        let Ok(resolved) = self.resolved() else {
            return Vec::new();
        };
        if !resolved.config.enabled {
            return Vec::new();
        }
        let classification = if self.public {
            RouteClassification::Public
        } else {
            RouteClassification::Unclassified
        };
        resolved
            .config
            .routes
            .iter()
            .map(|route| RouteInfo {
                method: "PROXY".to_owned(),
                path: format!("{}{}/*", route.host, normalize_prefix(&route.path_prefix)),
                handler: format!("{PLUGIN_NAME}::{}", route.name),
                classification,
                ..RouteInfo::default()
            })
            .collect()
    }

    /// The configuration after files, environment and code.
    ///
    /// # Errors
    ///
    /// The [`ConfigError`] that aborts boot.
    pub fn effective_config(&self) -> Result<&PingoraConfig, &ConfigError> {
        self.resolved().as_ref().map(|r| &r.config)
    }

    fn resolved(&self) -> &Result<Resolved, ConfigError> {
        self.resolved.get_or_init(|| {
            let mut resolved = match &self.explicit {
                Some(config) => Resolved::explicit((**config).clone()),
                None => PingoraConfig::resolve(DEFAULT_SECTION)?,
            };
            for apply in &self.overrides {
                apply(&mut resolved.config);
            }
            if let Some(development) = self.development {
                if development { "dev" } else { "prod" }.clone_into(&mut resolved.profile);
            }
            resolved.config.validate()?;
            Ok(resolved)
        })
    }
}

impl Plugin for PingoraPlugin {
    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed(PLUGIN_NAME)
    }

    fn build(self, app: AppBuilder) -> AppBuilder {
        let app = app.config_section(DEFAULT_SECTION);
        let shared = self.shared.clone();
        let (config, development) = match self.resolved() {
            Ok(resolved) => (resolved.config.clone(), resolved.is_development()),
            Err(error) => {
                return fail_at_startup(app, shared, &PingoraError::Config(error.clone()));
            }
        };
        if !config.enabled {
            tracing::info!("pingora proxy disabled by configuration");
            return app;
        }
        let declared = self.route_infos();
        let app = if config.metrics {
            app.metrics_source(PLUGIN_NAME, Arc::new(ProxyMetrics(shared.clone())))
        } else {
            app
        };
        let hook_shared = shared.clone();
        let stop_shared = shared.clone();
        app.declare_plugin_routes(declared)
            .health_indicator("pingora", Arc::new(ProxyHealth(shared)))
            .on_startup(move |state| {
                let shared = hook_shared.clone();
                let config = config.clone();
                async move {
                    match launch(&state, config, development, &shared).await {
                        Ok(addr) => {
                            tracing::info!(%addr, "pingora proxy listening");
                            state.insert_extension(PingoraHandle::new(shared.clone()));
                            drain_on_autumn_shutdown(&state, shared);
                            Ok(())
                        }
                        Err(error) => {
                            shared.fail();
                            Err(startup_error(&error.to_string()))
                        }
                    }
                }
            })
            .on_shutdown(move || {
                let handle = PingoraHandle::new(stop_shared.clone());
                async move { handle.shutdown().await }
            })
    }
}

/// Fail at startup, where Autumn reports hook errors.
fn fail_at_startup(app: AppBuilder, shared: Arc<Shared>, error: &PingoraError) -> AppBuilder {
    let message = error.to_string();
    app.on_startup(move |_state| {
        let shared = shared.clone();
        let message = message.clone();
        async move {
            shared.fail();
            Err(startup_error(&message))
        }
    })
}

fn startup_error(message: &str) -> autumn_web::AutumnError {
    autumn_web::AutumnError::internal_server_error_msg(format!("{PLUGIN_NAME}: {message}"))
}

/// Start the drain when Autumn's shutdown signal fires (ADR 0003).
fn drain_on_autumn_shutdown(state: &AppState, shared: Arc<Shared>) {
    let token = state.shutdown_token();
    tokio::spawn(async move {
        token.cancelled().await;
        PingoraHandle::new(shared).shutdown().await;
    });
}

async fn launch(
    state: &AppState,
    mut config: PingoraConfig,
    development: bool,
    shared: &Arc<Shared>,
) -> Result<SocketAddr, PingoraError> {
    if shared.lifecycle.get() != crate::Lifecycle::Idle {
        return Err(PingoraError::AlreadyStarted);
    }
    let autumn = state.config_arc();
    let bind = config.bind_addr(development)?;
    let fallback = match config.fallback {
        Fallback::App => Some(app_target(&autumn, bind).await?),
        Fallback::None => None,
    };
    if fallback.is_some() && !trusts_loopback(&autumn) {
        tracing::warn!(
            "pingora proxy: the Autumn app does not trust the proxy, so it sees 127.0.0.1 as the client; add 127.0.0.1/32 to `security.trusted_proxies.ranges` and set `trust_forwarded_headers = true`"
        );
    }
    fit_grace(&mut config, &autumn);

    let mut pools = Vec::with_capacity(config.routes.len());
    for route in &config.routes {
        pools.push(Pool::build(route, config.health_check_timeout()).await?);
    }
    let pools = Arc::new(pools);
    let route_names: Vec<String> = config.routes.iter().map(|r| r.name.clone()).collect();
    let metrics = config.metrics.then(|| {
        let metrics = Arc::new(Metrics::new(route_names.iter().map(String::as_str)));
        let _ = shared.metrics.set(metrics.clone());
        metrics
    });

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|source| PingoraError::Bind { addr: bind, source })?;

    let server_conf = Arc::new(ServerConf {
        // Pingora counts attempts, not retries.
        max_retries: config.max_retries.saturating_add(1),
        ..ServerConf::default()
    });
    let gateway = Gateway {
        router: Router::new(&config.routes),
        routes: config.routes.clone(),
        pools: pools.clone(),
        fallback,
        trust_forwarded: config.trust_forwarded_headers,
        max_body: config.max_request_body_bytes,
        timeouts: Timeouts {
            connect: config.connect_timeout(),
            read: config.read_timeout(),
            write: config.write_timeout(),
        },
        metrics,
    };
    let proxy = pingora_proxy::http_proxy(&server_conf, gateway);
    crate::server::start(
        shared,
        Launch {
            listener,
            proxy,
            pools,
            route_names,
            max_connections: config.max_connections,
            grace: config.shutdown_grace(),
            health_interval: config.health_check_interval(),
        },
    )
    .map_err(|source| PingoraError::Bind { addr: bind, source })
}

/// The Autumn app address for `fallback = "app"` (ADR 0002).
async fn app_target(autumn: &AutumnConfig, bind: SocketAddr) -> Result<SocketAddr, PingoraError> {
    if autumn.server.tls.is_some() {
        return Err(PingoraError::FallbackTls);
    }
    let host = autumn.server.host.trim();
    let port = autumn.server.port;
    let target = match host.parse::<IpAddr>() {
        Ok(ip) => SocketAddr::new(loopback_for(ip), port),
        Err(_) if host.eq_ignore_ascii_case("localhost") => {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
        }
        Err(_) => tokio::net::lookup_host((host, port))
            .await
            .ok()
            .and_then(|mut addrs| addrs.next())
            .ok_or_else(|| PingoraError::AppAddress(format!("{host}:{port}")))?,
    };
    if loops(bind, target) {
        return Err(PingoraError::FallbackLoop(target));
    }
    Ok(target)
}

/// An unspecified address (`0.0.0.0`, `::`) becomes loopback.
const fn loopback_for(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(v4) if v4.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(v6) if v6.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        other => other,
    }
}

/// `true` when the proxy listener would receive the fallback requests.
fn loops(bind: SocketAddr, target: SocketAddr) -> bool {
    bind.port() == target.port()
        && (bind.ip() == target.ip() || bind.ip().is_unspecified() && target.ip().is_loopback())
}

/// `true` when Autumn trusts forwarded headers from `127.0.0.1`.
fn trusts_loopback(autumn: &AutumnConfig) -> bool {
    let policy = &autumn.security.trusted_proxies;
    policy.trust_forwarded_headers
        && (policy.trusted_hops.is_some()
            || policy
                .ranges
                .iter()
                .any(|range| range_contains(range, IpAddr::V4(Ipv4Addr::LOCALHOST))))
}

/// `true` when `range` (an IP or CIDR) contains `ip`.
fn range_contains(range: &str, ip: IpAddr) -> bool {
    let (base, bits) = range
        .trim()
        .split_once('/')
        .map_or((range.trim(), None), |(base, bits)| (base, Some(bits)));
    let Ok(base) = base.parse::<IpAddr>() else {
        return false;
    };
    match (base, ip) {
        (IpAddr::V4(base), IpAddr::V4(ip)) => {
            let bits = bits.map_or(Some(32), |b| b.parse::<u32>().ok().filter(|b| *b <= 32));
            bits.is_some_and(|bits| {
                let mask = u32::MAX.checked_shl(32 - bits).unwrap_or(0);
                u32::from(base) & mask == u32::from(ip) & mask
            })
        }
        (IpAddr::V6(base), IpAddr::V6(ip)) => {
            let bits = bits.map_or(Some(128), |b| b.parse::<u32>().ok().filter(|b| *b <= 128));
            bits.is_some_and(|bits| {
                let mask = u128::MAX.checked_shl(128 - bits).unwrap_or(0);
                u128::from(base) & mask == u128::from(ip) & mask
            })
        }
        _ => false,
    }
}

/// Autumn runs plugin shutdown hooks inside `server.shutdown_timeout_secs`.
/// Cap the grace so the drain and the aborts fit.
fn fit_grace(config: &mut PingoraConfig, autumn: &AutumnConfig) {
    let budget = autumn.server.shutdown_timeout_secs.saturating_mul(1000);
    let limit = budget.saturating_sub(KILL_WAIT_MS.min(budget / 2)).max(1);
    if config.shutdown_grace_ms > limit {
        tracing::warn!(
            shutdown_grace_ms = config.shutdown_grace_ms,
            limit_ms = limit,
            "pingora shutdown_grace_ms does not fit in server.shutdown_timeout_secs; using the limit"
        );
        config.shutdown_grace_ms = limit;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn autumn(host: &str, port: u16) -> AutumnConfig {
        let mut config = AutumnConfig::default();
        host.clone_into(&mut config.server.host);
        config.server.port = port;
        config
    }

    #[tokio::test]
    async fn the_app_target_uses_loopback_for_unspecified_hosts() {
        let bind: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        for (host, want) in [
            ("0.0.0.0", "127.0.0.1:3000"),
            ("::", "[::1]:3000"),
            ("localhost", "127.0.0.1:3000"),
            ("10.1.2.3", "10.1.2.3:3000"),
        ] {
            let target = app_target(&autumn(host, 3000), bind).await.unwrap();
            assert_eq!(target.to_string(), want, "{host}");
        }
        let error = app_target(&autumn("no-such-host.invalid", 3000), bind)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("no-such-host.invalid"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn tls_on_the_app_is_an_error() {
        let mut config = autumn("127.0.0.1", 3000);
        config.server.tls =
            Some(toml::from_str("cert_path = \"c.pem\"\nkey_path = \"k.pem\"").unwrap());
        let error = app_target(&config, "127.0.0.1:8080".parse().unwrap())
            .await
            .unwrap_err();
        assert!(matches!(error, PingoraError::FallbackTls), "{error}");
    }

    #[test]
    fn loops_are_found() {
        let at = |s: &str| s.parse::<SocketAddr>().unwrap();
        assert!(loops(at("127.0.0.1:3000"), at("127.0.0.1:3000")));
        assert!(loops(at("0.0.0.0:3000"), at("127.0.0.1:3000")));
        assert!(!loops(at("0.0.0.0:8080"), at("127.0.0.1:3000")));
        assert!(!loops(at("127.0.0.2:3000"), at("127.0.0.1:3000")));
    }

    #[test]
    fn ranges_contain_addresses() {
        let lo = IpAddr::V4(Ipv4Addr::LOCALHOST);
        for range in ["127.0.0.1", "127.0.0.0/8", "0.0.0.0/0", " 127.0.0.1/32 "] {
            assert!(range_contains(range, lo), "{range}");
        }
        for range in ["10.0.0.0/8", "127.0.0.2", "::1", "junk", "127.0.0.0/40"] {
            assert!(!range_contains(range, lo), "{range}");
        }
        assert!(range_contains("::/0", IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(range_contains("::1/128", IpAddr::V6(Ipv6Addr::LOCALHOST)));
    }

    #[test]
    fn trust_needs_the_switch_and_a_range_or_hops() {
        let mut config = AutumnConfig::default();
        config.security.trusted_proxies.trust_forwarded_headers = false;
        config.security.trusted_proxies.ranges = vec!["127.0.0.1".to_owned()];
        assert!(!trusts_loopback(&config));
        config.security.trusted_proxies.trust_forwarded_headers = true;
        assert!(trusts_loopback(&config));
        config.security.trusted_proxies.ranges.clear();
        assert!(!trusts_loopback(&config));
        config.security.trusted_proxies.trusted_hops = Some(1);
        assert!(trusts_loopback(&config));
    }

    #[test]
    fn the_grace_fits_the_autumn_budget() {
        let mut autumn = AutumnConfig::default();
        autumn.server.shutdown_timeout_secs = 2;
        let mut config = PingoraConfig::default();
        fit_grace(&mut config, &autumn);
        assert_eq!(config.shutdown_grace_ms, 1_500);
        config.shutdown_grace_ms = 100;
        fit_grace(&mut config, &autumn);
        assert_eq!(config.shutdown_grace_ms, 100);
        autumn.server.shutdown_timeout_secs = 0;
        fit_grace(&mut config, &autumn);
        assert_eq!(config.shutdown_grace_ms, 1);
    }
}
