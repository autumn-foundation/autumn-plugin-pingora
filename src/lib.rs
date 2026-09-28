//! A [Pingora](https://github.com/cloudflare/pingora) reverse proxy for
//! [Autumn](https://autumn-web.app).

#![forbid(unsafe_code)]

mod config;
#[cfg_attr(not(test), expect(dead_code, reason = "the proxy uses it (slice 2)"))]
mod forwarded;
mod lifecycle;
#[cfg_attr(not(test), expect(dead_code, reason = "the proxy uses it (slice 2)"))]
mod router;

pub use config::{
    ConfigError, DEFAULT_PORT, DEFAULT_SECTION, Fallback, PingoraConfig, Resolved, RouteConfig,
    Selection,
};
pub use lifecycle::{Lifecycle, LifecycleCell, LifecycleEvent};
