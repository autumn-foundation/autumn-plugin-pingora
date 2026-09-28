//! A [Pingora](https://github.com/cloudflare/pingora) reverse proxy for
//! [Autumn](https://autumn-web.app).

#![forbid(unsafe_code)]

mod config;
mod forwarded;
mod lifecycle;
mod router;

pub use config::{
    ConfigError, DEFAULT_PORT, DEFAULT_SECTION, Fallback, PingoraConfig, Resolved, RouteConfig,
    Selection,
};
pub use lifecycle::{Lifecycle, LifecycleCell, LifecycleEvent};
