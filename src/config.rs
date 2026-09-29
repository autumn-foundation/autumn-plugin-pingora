//! The `[pingora]` section of `autumn.toml`.
//!
//! Each key has a safe default. The plugin merges the section in the same
//! order as Autumn core:
//!
//! 1. `[pingora]` in `autumn.toml`.
//! 2. `[profile.<name>.pingora]` in `autumn.toml`.
//! 3. `[pingora]` in `autumn-<profile>.toml`.
//! 4. `AUTUMN_PINGORA__*` environment variables (also from `.env` files).
//!
//! ```toml
//! [pingora]
//! bind = "0.0.0.0:8080"
//! fallback = "app"           # unmatched requests go to the Autumn app
//!
//! [[pingora.routes]]
//! name = "billing"
//! path_prefix = "/billing"
//! upstreams = ["10.0.0.7:8080", "10.0.0.8:8080"]
//! strip_prefix = true
//! ```

use std::collections::HashSet;
use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

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
    /// Respond 404. In TOML: `"none"`.
    #[serde(rename = "none")]
    NotFound,
}

impl Fallback {
    /// The name in TOML.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::NotFound => "none",
        }
    }
}

impl fmt::Display for Fallback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
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
pub struct Route {
    /// Unique name, used in metrics and logs. Letters, digits, `_`, `-`
    /// and `.` only. `fallback` and `unmatched` are reserved.
    pub name: String,
    /// Host to match: `api.example.com` or `*.example.com`. Empty: any
    /// host. The match ignores case and the port.
    pub host: String,
    /// Path prefix to match, on a segment boundary: `/api` matches `/api`
    /// and `/api/x`, not `/apix`. Default: `/`.
    pub path_prefix: String,
    /// Upstream addresses, `host:port`. Hostnames resolve at boot.
    pub upstreams: Vec<String>,
    /// Remove `path_prefix` before the request goes upstream.
    /// Default: `false`.
    pub strip_prefix: bool,
    /// `Host` header for the upstream. Empty: keep the client `Host`.
    pub upstream_host: String,
    /// How to pick an upstream. Default: `round_robin`.
    pub selection: Selection,
}

impl Default for Route {
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
///
/// For a setting that says "`0`: off", the value `0` removes that limit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct PingoraConfig {
    /// Start the proxy. Default: `true`.
    pub enabled: bool,
    /// Listen address, `IP:port`. Port `0` picks a free port. Empty: the
    /// profile decides. `dev` and `test` use `127.0.0.1:8080`. Other
    /// profiles use `0.0.0.0:8080`. Default: empty.
    pub bind: String,
    /// Where unmatched requests go: `app` (the Autumn app) or `none`
    /// (404). Default: `app`.
    pub fallback: Fallback,
    /// Peers (IPs or CIDR ranges) whose `X-Forwarded-*` and other identity
    /// headers the proxy keeps. For other peers, the proxy replaces them.
    /// Put only your own load balancers here. Default: empty.
    pub trusted_proxies: Vec<String>,
    /// Upstream connect timeout. Must be more than `0`. Default: `5000`.
    pub connect_timeout_ms: u64,
    /// Upstream read timeout. `0`: off. Default: `60000`.
    pub read_timeout_ms: u64,
    /// Upstream write timeout. `0`: off. Default: `60000`.
    pub write_timeout_ms: u64,
    /// Connect retries on another upstream. Pingora also retries an
    /// idempotent request once or twice when a pooled connection is stale.
    /// Default: `1`.
    pub max_retries: usize,
    /// Upstream TCP health check interval. `0`: off. Default: `5000`.
    pub health_check_interval_ms: u64,
    /// Health check connect timeout. Must be more than `0`.
    /// Default: `1000`.
    pub health_check_timeout_ms: u64,
    /// Largest request body. Bigger requests get 413. `0`: off.
    /// Default: `10485760` (10 MiB).
    pub max_request_body_bytes: u64,
    /// Maximum open client connections. At the limit, the proxy stops
    /// accepting until a connection closes. `0`: no limit. At most
    /// `1000000`. Default: `10000`.
    pub max_connections: usize,
    /// Maximum open connections from one client IP. The proxy closes
    /// connections over the limit at once. Keep `0` behind a load balancer,
    /// because all connections then have its IP. `0`: no limit.
    /// Default: `0`.
    pub max_connections_per_ip: usize,
    /// Time for open requests to finish at shutdown. Must be more than
    /// `0`. The plugin caps it to fit `server.shutdown_timeout_secs`.
    /// Default: `10000`.
    pub shutdown_grace_ms: u64,
    /// Record `pingora_proxy_*` metrics. Default: `true`.
    pub metrics: bool,
    /// The routes. Env: `AUTUMN_PINGORA__ROUTES` replaces all routes with
    /// a TOML array.
    pub routes: Vec<Route>,
}

impl Default for PingoraConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bind: String::new(),
            fallback: Fallback::App,
            trusted_proxies: Vec::new(),
            connect_timeout_ms: 5_000,
            read_timeout_ms: 60_000,
            write_timeout_ms: 60_000,
            max_retries: 1,
            health_check_interval_ms: 5_000,
            health_check_timeout_ms: 1_000,
            max_request_body_bytes: 10 * 1024 * 1024,
            max_connections: 10_000,
            max_connections_per_ip: 0,
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
    pub fn is_development(&self) -> bool {
        matches!(self.profile.as_str(), "dev" | "test")
    }

    /// Configuration from code. The plugin reads no files. The profile
    /// still comes from the environment.
    pub(crate) fn explicit(config: PingoraConfig) -> Self {
        let profile = autumn_web::dotenv::os_env_with_dotenv().map_or_else(
            |_| resolve_active_profile(&autumn_web::config::OsEnv).1,
            |env| resolve_active_profile(&env).1,
        );
        Self { config, profile }
    }
}

const fn millis(ms: u64) -> Option<Duration> {
    if ms == 0 {
        None
    } else {
        Some(Duration::from_millis(ms))
    }
}

/// Largest `max_connections`.
pub const MAX_CONNECTIONS_LIMIT: usize = 1_000_000;

/// Names that metrics use for requests with no route.
pub const RESERVED_NAMES: [&str; 2] = ["fallback", "unmatched"];

impl PingoraConfig {
    /// The listen address. An empty `bind` gives `127.0.0.1:8080` in
    /// development and `0.0.0.0:8080` in other profiles.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when `bind` is not `IP:port`.
    pub fn bind_addr(&self, development: bool) -> Result<SocketAddr, ConfigError> {
        let bind = self.bind.trim();
        if bind.is_empty() {
            let host = if development {
                [127, 0, 0, 1]
            } else {
                [0, 0, 0, 0]
            };
            return Ok(SocketAddr::from((host, DEFAULT_PORT)));
        }
        bind.parse().map_err(|_| {
            ConfigError(format!(
                "`bind` must be IP:port (for example \"0.0.0.0:8080\"), found \"{}\"",
                self.bind
            ))
        })
    }

    /// Upstream connect timeout.
    #[must_use]
    pub const fn connect_timeout(&self) -> Duration {
        Duration::from_millis(self.connect_timeout_ms)
    }

    /// Upstream read timeout.
    #[must_use]
    pub const fn read_timeout(&self) -> Option<Duration> {
        millis(self.read_timeout_ms)
    }

    /// Upstream write timeout.
    #[must_use]
    pub const fn write_timeout(&self) -> Option<Duration> {
        millis(self.write_timeout_ms)
    }

    /// Health check interval.
    #[must_use]
    pub const fn health_check_interval(&self) -> Option<Duration> {
        millis(self.health_check_interval_ms)
    }

    /// Health check connect timeout.
    #[must_use]
    pub const fn health_check_timeout(&self) -> Duration {
        Duration::from_millis(self.health_check_timeout_ms)
    }

    /// Shutdown grace period.
    #[must_use]
    pub const fn shutdown_grace(&self) -> Duration {
        Duration::from_millis(self.shutdown_grace_ms)
    }

    /// Parse `[section]` from a whole `autumn.toml` text. No profiles and
    /// no environment.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] for bad TOML, unknown keys, wrong types or
    /// values that [`validate`](Self::validate) rejects.
    pub fn from_toml_str(text: &str, section: &str) -> Result<Self, ConfigError> {
        let document: toml::Table = toml::from_str(text).map_err(|e| ConfigError(e.to_string()))?;
        let config = Self::from_section(document.get(section))?;
        config.validate()?;
        Ok(config)
    }

    fn from_section(section: Option<&toml::Value>) -> Result<Self, ConfigError> {
        section.map_or_else(
            || Ok(Self::default()),
            |value| {
                value
                    .clone()
                    .try_into()
                    .map_err(|e: toml::de::Error| ConfigError(e.to_string()))
            },
        )
    }

    /// Resolve `[section]` from the app's files and the process environment.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when a file cannot be read or parsed, or when
    /// the merged section is not valid.
    pub fn resolve(section: &str) -> Result<Resolved, ConfigError> {
        let env = autumn_web::dotenv::os_env_with_dotenv()
            .map_err(|e| ConfigError(format!("cannot read .env files: {e}")))?;
        Self::resolve_with_env(section, &env)
    }

    /// Like [`resolve`](Self::resolve), but reads only `env`.
    ///
    /// # Errors
    ///
    /// See [`resolve`](Self::resolve).
    pub fn resolve_with_env(section: &str, env: &dyn Env) -> Result<Resolved, ConfigError> {
        let (selected, canonical) = resolve_active_profile(env);
        let mut merged = toml::Value::Table(toml::map::Map::new());

        if let Some(base) = read_optional_toml(&find_config_file("autumn.toml", env))? {
            deep_merge(&mut merged, base.clone());
            for name in profile_inline_lookup_names(&canonical) {
                if let Some(profile) = profile_section(&base, name) {
                    deep_merge(&mut merged, profile);
                }
            }
        }
        for name in autumn_web::config::profile_override_file_lookup_names(&canonical, &selected) {
            let path = find_config_file(&format!("autumn-{name}.toml"), env);
            if let Some(overlay) = read_optional_toml(&path)? {
                deep_merge(&mut merged, overlay);
                break;
            }
        }

        let mut section_value = merged
            .get(section)
            .cloned()
            .unwrap_or_else(|| toml::Value::Table(toml::map::Map::new()));
        if !section_value.is_table() {
            return Err(ConfigError(format!("`{section}` must be a table")));
        }
        let mut config = Self::from_section(Some(&section_value))?;
        apply_env_overrides(section, &mut section_value, &mut config, env)?;
        config.validate()?;
        Ok(Resolved {
            config,
            profile: canonical,
        })
    }

    /// Reject values that fail at runtime.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] that names the bad key.
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.bind_addr(true)?;
        for (key, value) in [
            ("shutdown_grace_ms", self.shutdown_grace_ms),
            ("connect_timeout_ms", self.connect_timeout_ms),
            ("health_check_timeout_ms", self.health_check_timeout_ms),
        ] {
            if value == 0 {
                return Err(ConfigError(format!("`{key}` must be more than 0")));
            }
        }
        for range in &self.trusted_proxies {
            if crate::forwarded::IpRange::parse(range).is_none() {
                return Err(ConfigError(format!(
                    "`trusted_proxies` entry \"{range}\" must be an IP or a CIDR range"
                )));
            }
        }
        if self.max_connections > MAX_CONNECTIONS_LIMIT {
            return Err(ConfigError(format!(
                "`max_connections` must be at most {MAX_CONNECTIONS_LIMIT}"
            )));
        }
        let mut names = HashSet::new();
        let mut rules = HashSet::new();
        for route in &self.routes {
            route.validate()?;
            if !names.insert(route.name.as_str()) {
                return Err(ConfigError(format!(
                    "two routes have the name `{}`",
                    route.name
                )));
            }
            let rule = (
                route.host.to_ascii_lowercase(),
                normalize_prefix(&route.path_prefix),
            );
            if !rules.insert(rule) {
                return Err(ConfigError(format!(
                    "route `{}` has the same host and path_prefix as an earlier route",
                    route.name
                )));
            }
        }
        Ok(())
    }
}

/// A prefix without a trailing `/`. The root prefix is empty.
pub fn normalize_prefix(prefix: &str) -> String {
    let trimmed = prefix.trim();
    let trimmed = if trimmed.is_empty() { "/" } else { trimmed };
    trimmed.trim_end_matches('/').to_owned()
}

impl Route {
    /// A route with `name`, the root prefix and no upstreams.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }

    /// Match this host: `api.example.com` or `*.example.com`.
    #[must_use]
    pub fn host(mut self, host: impl Into<String>) -> Self {
        self.host = host.into();
        self
    }

    /// Match this path prefix.
    #[must_use]
    pub fn path_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.path_prefix = prefix.into();
        self
    }

    /// Add an upstream, `host:port`.
    #[must_use]
    pub fn upstream(mut self, upstream: impl Into<String>) -> Self {
        self.upstreams.push(upstream.into());
        self
    }

    /// Add upstreams, `host:port`.
    #[must_use]
    pub fn upstreams<I, S>(mut self, upstreams: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.upstreams.extend(upstreams.into_iter().map(Into::into));
        self
    }

    /// Remove the prefix before the request goes upstream.
    #[must_use]
    pub const fn strip_prefix(mut self, strip: bool) -> Self {
        self.strip_prefix = strip;
        self
    }

    /// Send this `Host` header upstream.
    #[must_use]
    pub fn upstream_host(mut self, host: impl Into<String>) -> Self {
        self.upstream_host = host.into();
        self
    }

    /// Pick upstreams with `selection`.
    #[must_use]
    pub const fn selection(mut self, selection: Selection) -> Self {
        self.selection = selection;
        self
    }

    /// Reject a route that cannot work.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] that names the route and the key.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let fail = |message: String| Err(ConfigError(format!("route `{}`: {message}", self.name)));
        let name_ok = !self.name.is_empty()
            && self
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
        if !name_ok {
            return fail(
                "`name` must be letters, digits, `_`, `-` or `.`, and not empty".to_owned(),
            );
        }
        if RESERVED_NAMES.contains(&self.name.as_str()) {
            return fail(format!("the name `{}` is reserved", self.name));
        }
        if !valid_host_pattern(&self.host) {
            return fail(format!(
                "`host` must be a host name or `*.domain`, without a port, found \"{}\"",
                self.host
            ));
        }
        let prefix = self.path_prefix.trim();
        if !prefix.starts_with('/')
            || prefix.contains(['?', '#'])
            || prefix.chars().any(|c| c.is_whitespace() || c.is_control())
            || crate::router::unsafe_path(prefix)
        {
            return fail(format!(
                "`path_prefix` must start with `/` and have no query, fragment or dot segment, found \"{}\"",
                self.path_prefix
            ));
        }
        if self.upstreams.is_empty() {
            return fail("`upstreams` must have at least one address".to_owned());
        }
        let mut seen = HashSet::new();
        for upstream in &self.upstreams {
            if !valid_upstream(upstream) {
                return fail(format!(
                    "upstream \"{upstream}\" must be host:port (for example \"10.0.0.7:8080\")"
                ));
            }
            if !seen.insert(upstream.to_ascii_lowercase()) {
                return fail(format!("upstream \"{upstream}\" is in the list two times"));
            }
        }
        if http::HeaderValue::from_str(&self.upstream_host).is_err()
            || self.upstream_host.chars().any(char::is_whitespace)
        {
            return fail("`upstream_host` must be a valid Host header value".to_owned());
        }
        Ok(())
    }
}

/// `host:port`, where host is a name, an IPv4 address or `[IPv6]`.
fn valid_upstream(upstream: &str) -> bool {
    if upstream.parse::<SocketAddr>().is_ok() {
        return true;
    }
    let Some((host, port)) = upstream.rsplit_once(':') else {
        return false;
    };
    port.parse::<u16>().is_ok_and(|p| p > 0) && valid_host_name(host)
}

/// Letters, digits, `-` and `.`, in labels that are not empty.
fn valid_host_name(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

/// Empty, a host name, `[IPv6]`, or `*.` and a host name.
fn valid_host_pattern(pattern: &str) -> bool {
    if pattern.is_empty() {
        return true;
    }
    if let Some(inner) = pattern.strip_prefix('[').and_then(|p| p.strip_suffix(']')) {
        return inner.parse::<std::net::Ipv6Addr>().is_ok();
    }
    let name = pattern.strip_prefix("*.").unwrap_or(pattern);
    valid_host_name(name)
}

/// The env prefix for a section: `pingora` → `AUTUMN_PINGORA__`.
fn env_prefix(section: &str) -> String {
    format!("AUTUMN_{}__", section.to_ascii_uppercase())
}

/// Apply `AUTUMN_<SECTION>__<KEY>` overrides for each known key.
///
/// Each value is a TOML literal (`true`, `10`, `"x"`, `[..]`) or a bare
/// string. A wrong type causes an error. The file value does not stay.
/// The caller validates the result once, after all overrides.
fn apply_env_overrides(
    section_name: &str,
    section: &mut toml::Value,
    config: &mut PingoraConfig,
    env: &dyn Env,
) -> Result<(), ConfigError> {
    let defaults =
        toml::Value::try_from(PingoraConfig::default()).map_err(|e| ConfigError(e.to_string()))?;
    let prefix = env_prefix(section_name);
    let Some(keys) = defaults.as_table() else {
        return Ok(());
    };
    for name in keys.keys() {
        let key = format!("{prefix}{}", name.to_ascii_uppercase());
        let Some(raw) = env_trimmed(env, &key) else {
            continue;
        };
        let mut value = parse_env_value(&raw);
        // Enum names are lower case in TOML. Accept `App` or `NONE` too.
        if name == "fallback"
            && let toml::Value::String(text) = &value
        {
            value = toml::Value::String(text.to_ascii_lowercase());
        }
        let mut candidate = section.clone();
        if let Some(table) = candidate.as_table_mut() {
            table.insert(name.clone(), value);
        }
        *config = PingoraConfig::from_section(Some(&candidate))
            .map_err(|error| ConfigError(format!("{key}: {}", error.message())))?;
        *section = candidate;
    }
    Ok(())
}

fn parse_env_value(raw: &str) -> toml::Value {
    toml::from_str::<toml::Table>(&format!("v = {raw}"))
        .ok()
        .and_then(|mut table| table.remove("v"))
        .unwrap_or_else(|| toml::Value::String(raw.to_owned()))
}

/// A non-blank env value. A blank value counts as unset, as in core.
fn env_trimmed(env: &dyn Env, key: &str) -> Option<String> {
    env.var(key)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// `(selected spelling, canonical name)` of the active profile.
///
/// Same order as core: `AUTUMN_ENV`, `AUTUMN_PROFILE`, `--profile`,
/// `AUTUMN_IS_DEBUG=0` (→ `prod`), then `dev`.
fn resolve_active_profile(env: &dyn Env) -> (String, String) {
    let selected = resolve_profile_input(env);
    let canonical =
        autumn_web::config::normalize_profile_name(&selected).unwrap_or_else(|| "dev".to_owned());
    (selected, canonical)
}

fn resolve_profile_input(env: &dyn Env) -> String {
    if let Some(value) = env_trimmed(env, "AUTUMN_ENV") {
        return value;
    }
    if let Some(value) = env_trimmed(env, "AUTUMN_PROFILE") {
        return value;
    }
    let args: Vec<String> = std::env::args().collect();
    for (index, arg) in args.iter().enumerate() {
        if arg == "--profile"
            && let Some(profile) = args.get(index.saturating_add(1))
            && !profile.trim().is_empty()
        {
            return profile.trim().to_owned();
        }
        if let Some(profile) = arg.strip_prefix("--profile=")
            && !profile.trim().is_empty()
        {
            return profile.trim().to_owned();
        }
    }
    if env_trimmed(env, "AUTUMN_IS_DEBUG").as_deref() == Some("0") {
        return "prod".to_owned();
    }
    "dev".to_owned()
}

/// Find a config file as core does: `AUTUMN_MANIFEST_DIR` first, then the
/// working directory.
fn find_config_file(filename: &str, env: &dyn Env) -> PathBuf {
    if let Some(dir) = env_trimmed(env, "AUTUMN_MANIFEST_DIR") {
        let candidate = PathBuf::from(dir).join(filename);
        if candidate.exists() {
            return candidate;
        }
    }
    PathBuf::from(filename)
}

fn read_optional_toml(path: &Path) -> Result<Option<toml::Value>, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(contents) => {
            let table = toml::from_str::<toml::Table>(&contents)
                .map_err(|e| ConfigError(format!("cannot parse {}: {e}", path.display())))?;
            Ok(Some(toml::Value::Table(table)))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(ConfigError(format!(
            "cannot read {}: {error}",
            path.display()
        ))),
    }
}

fn profile_inline_lookup_names(canonical: &str) -> Vec<&str> {
    match canonical {
        "prod" => vec!["production", "prod"],
        "dev" => vec!["development", "dev"],
        other => vec![other],
    }
}

fn profile_section(base: &toml::Value, profile: &str) -> Option<toml::Value> {
    base.get("profile")
        .and_then(toml::Value::as_table)
        .and_then(|profiles| profiles.get(profile))
        .and_then(toml::Value::as_table)
        .map(|table| toml::Value::Table(table.clone()))
}

/// Merge `overlay` into `base`. Tables merge; other values replace.
fn deep_merge(base: &mut toml::Value, overlay: toml::Value) {
    deep_merge_at(base, overlay, 0);
}

fn deep_merge_at(base: &mut toml::Value, overlay: toml::Value, depth: usize) {
    const MAX_MERGE_DEPTH: usize = 16;
    if depth > MAX_MERGE_DEPTH {
        return;
    }
    let toml::Value::Table(overlay_table) = overlay else {
        return;
    };
    let Some(base_table) = base.as_table_mut() else {
        return;
    };
    for (key, overlay_value) in overlay_table {
        let recurse =
            overlay_value.is_table() && base_table.get(&key).is_some_and(toml::Value::is_table);
        if recurse {
            if let Some(base_value) = base_table.get_mut(&key) {
                deep_merge_at(base_value, overlay_value, depth.saturating_add(1));
            }
        } else {
            base_table.insert(key, overlay_value);
        }
    }
}
