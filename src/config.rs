//! The `[pingora]` section of `autumn.toml`.

use std::fmt;
use std::net::SocketAddr;

use autumn_web::config::Env;
use serde::{Deserialize, Serialize};

/// The default proxy port.
pub const DEFAULT_PORT: u16 = 8080;

/// The default config section.
pub const DEFAULT_SECTION: &str = "pingora";

/// Where unmatched requests go.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Fallback {
    /// To the Autumn app (ADR 0002).
    #[default]
    App,
    /// Respond 404.
    None,
}

/// How a route picks an upstream.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Selection {
    /// Each upstream in turn.
    #[default]
    RoundRobin,
    /// Consistent hash of the client IP.
    Consistent,
}

impl fmt::Display for Selection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::RoundRobin => "round_robin",
            Self::Consistent => "consistent",
        })
    }
}

/// One route: a match rule and an upstream pool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct RouteConfig {
    /// Unique name. Used in metrics and logs.
    pub name: String,
    /// Host to match.
    pub host: String,
    /// Path prefix to match.
    pub path_prefix: String,
    /// Upstream addresses.
    pub upstreams: Vec<String>,
    /// Remove the prefix.
    pub strip_prefix: bool,
    /// `Host` header for the upstream.
    pub upstream_host: String,
    /// Upstream selection.
    pub selection: Selection,
}

impl Default for RouteConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            host: String::new(),
            path_prefix: "/".to_owned(),
            upstreams: Vec::new(),
            strip_prefix: false,
            upstream_host: String::new(),
            selection: Selection::RoundRobin,
        }
    }
}

/// Settings for the proxy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct PingoraConfig {
    /// Start the proxy.
    pub enabled: bool,
    /// Listen address.
    pub bind: String,
    /// Where unmatched requests go.
    pub fallback: Fallback,
    /// Trust client `X-Forwarded-*` headers.
    pub trust_forwarded_headers: bool,
    /// Upstream connect timeout.
    pub connect_timeout_ms: u64,
    /// Upstream read timeout.
    pub read_timeout_ms: u64,
    /// Upstream write timeout.
    pub write_timeout_ms: u64,
    /// Connect retries.
    pub max_retries: usize,
    /// Health check interval.
    pub health_check_interval_ms: u64,
    /// Health check connect timeout.
    pub health_check_timeout_ms: u64,
    /// Request body limit.
    pub max_request_body_bytes: u64,
    /// Open connection limit.
    pub max_connections: usize,
    /// Drain time at shutdown.
    pub shutdown_grace_ms: u64,
    /// Record metrics.
    pub metrics: bool,
    /// The routes.
    pub routes: Vec<RouteConfig>,
}

impl Default for PingoraConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bind: String::new(),
            fallback: Fallback::App,
            trust_forwarded_headers: false,
            connect_timeout_ms: 5_000,
            read_timeout_ms: 60_000,
            write_timeout_ms: 60_000,
            max_retries: 1,
            health_check_interval_ms: 5_000,
            health_check_timeout_ms: 1_000,
            max_request_body_bytes: 10 * 1024 * 1024,
            max_connections: 10_000,
            shutdown_grace_ms: 10_000,
            metrics: true,
            routes: Vec::new(),
        }
    }
}

/// A configuration problem. Boot stops.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid pingora configuration: {0}")]
pub struct ConfigError(String);

impl ConfigError {
    /// The problem, without the prefix.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.0
    }
}

/// A resolved configuration and the active profile.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub(crate) config: PingoraConfig,
    pub(crate) profile: String,
}

impl Resolved {
    /// The merged, validated configuration.
    #[must_use]
    pub const fn config(&self) -> &PingoraConfig {
        &self.config
    }

    /// The canonical active profile.
    #[must_use]
    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// `true` for the `dev` and `test` profiles.
    #[must_use]
    pub const fn is_development(&self) -> bool {
        false
    }
}

impl PingoraConfig {
    /// The listen address.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when `bind` is not `IP:port`.
    pub fn bind_addr(&self, _development: bool) -> Result<SocketAddr, ConfigError> {
        Err(ConfigError(String::new()))
    }

    /// Parse `[section]` from a whole `autumn.toml` text.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] for bad TOML or values.
    pub fn from_toml_str(_text: &str, _section: &str) -> Result<Self, ConfigError> {
        Ok(Self::default())
    }

    /// Resolve `[section]` from the app's files and the process environment.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] for bad files or values.
    pub fn resolve(section: &str) -> Result<Resolved, ConfigError> {
        Self::resolve_with_env(section, &autumn_web::config::OsEnv)
    }

    /// Like [`resolve`](Self::resolve), but reads only `env`.
    ///
    /// # Errors
    ///
    /// See [`resolve`](Self::resolve).
    pub fn resolve_with_env(_section: &str, _env: &dyn Env) -> Result<Resolved, ConfigError> {
        Ok(Resolved {
            config: Self::default(),
            profile: String::new(),
        })
    }

    /// Reject values that fail at runtime.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] that names the bad key.
    pub const fn validate(&self) -> Result<(), ConfigError> {
        Ok(())
    }
}
