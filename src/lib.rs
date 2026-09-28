//! A [Pingora](https://github.com/cloudflare/pingora) reverse proxy for
//! [Autumn](https://autumn-web.app).

#![forbid(unsafe_code)]

mod config;
#[cfg_attr(not(test), expect(dead_code, reason = "the proxy uses it (slice 2)"))]
mod forwarded;
mod lifecycle;
mod plugin;
#[cfg_attr(not(test), expect(dead_code, reason = "the proxy uses it (slice 2)"))]
mod router;
mod server;

pub use config::{
    ConfigError, DEFAULT_PORT, DEFAULT_SECTION, Fallback, PingoraConfig, Resolved, Route, Selection,
};
pub use lifecycle::{Lifecycle, LifecycleCell, LifecycleEvent};
pub use plugin::{PLUGIN_NAME, PingoraPlugin};
pub use server::PingoraHandle;
