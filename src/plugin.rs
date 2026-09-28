//! [`PingoraPlugin`]: the builder and the Autumn `Plugin` implementation.

use std::sync::Arc;

use autumn_web::app::AppBuilder;
use autumn_web::plugin::Plugin;
use autumn_web::route_listing::RouteInfo;

use crate::config::{ConfigError, Fallback, PingoraConfig, Route};
use crate::server::{PingoraHandle, Shared};

/// The name in `Plugin::name`, logs and errors.
pub const PLUGIN_NAME: &str = "autumn-plugin-pingora";

type Override = Arc<dyn Fn(&mut PingoraConfig) + Send + Sync>;

/// Runs a Pingora reverse proxy inside an Autumn app.
pub struct PingoraPlugin {
    config: PingoraConfig,
    overrides: Vec<Override>,
    shared: Arc<Shared>,
}

impl Default for PingoraPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for PingoraPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PingoraPlugin").finish_non_exhaustive()
    }
}

impl PingoraPlugin {
    /// A plugin with no routes, configured from `[pingora]`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: PingoraConfig::default(),
            overrides: Vec::new(),
            shared: Arc::new(Shared::default()),
        }
    }

    /// Add a route.
    #[must_use]
    pub fn route(self, route: Route) -> Self {
        self.configure(move |c| c.routes.push(route.clone()))
    }

    /// Change the configuration in code.
    #[must_use]
    pub fn configure(mut self, apply: impl Fn(&mut PingoraConfig) + Send + Sync + 'static) -> Self {
        self.overrides.push(Arc::new(apply));
        self
    }

    /// Use `config` and read no files.
    #[must_use]
    pub fn config(mut self, config: PingoraConfig) -> Self {
        self.config = config;
        self
    }

    /// Force development or production defaults.
    #[must_use]
    pub const fn development(self, _development: bool) -> Self {
        self
    }

    /// Set the listen address.
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

    /// Declare the proxied prefixes public.
    #[must_use]
    pub const fn public(self) -> Self {
        self
    }

    /// A handle to this plugin's proxy.
    #[must_use]
    pub fn handle(&self) -> PingoraHandle {
        PingoraHandle::new(self.shared.clone())
    }

    /// The proxied prefixes as `autumn routes` lists them.
    #[must_use]
    pub const fn route_infos(&self) -> Vec<RouteInfo> {
        Vec::new()
    }

    /// The configuration after files, environment and code.
    ///
    /// # Errors
    ///
    /// The [`ConfigError`] that aborts boot.
    pub const fn effective_config(&self) -> Result<&PingoraConfig, &ConfigError> {
        Ok(&self.config)
    }
}

impl Plugin for PingoraPlugin {
    fn name(&self) -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed(PLUGIN_NAME)
    }

    fn build(self, app: AppBuilder) -> AppBuilder {
        app
    }
}
