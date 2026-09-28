//! A [Pingora](https://github.com/cloudflare/pingora) reverse proxy for
//! [Autumn](https://autumn-web.app).
//!
//! ```rust,ignore
//! use autumn_plugin_pingora::{PingoraPlugin, Route};
//!
//! #[autumn_web::main]
//! async fn main() {
//!     autumn_web::app()
//!         .routes(routes![index])
//!         .plugin(PingoraPlugin::new().route(
//!             Route::new("billing").path_prefix("/billing").upstream("10.0.0.7:8080"),
//!         ))
//!         .run()
//!         .await;
//! }
//! ```
//!
//! The proxy has its own listener (default `127.0.0.1:8080` in dev,
//! `0.0.0.0:8080` in other profiles). It sends matched requests to the
//! route's upstream pool and all other requests to the Autumn app. It adds:
//!
//! - round-robin or consistent-hash load balancing,
//! - TCP health checks and connect retries,
//! - `X-Forwarded-*` headers,
//! - body and connection limits,
//! - a `pingora` indicator in `/actuator/health`,
//! - `pingora_proxy_*` metrics in `/actuator/prometheus`,
//! - graceful drain on Autumn's shutdown signal.
//!
//! See [`PingoraConfig`] for the `[pingora]` section of `autumn.toml`.

#![forbid(unsafe_code)]

mod config;
mod error;
mod forwarded;
mod health;
mod lifecycle;
mod metrics;
mod plugin;
mod proxy;
mod router;
mod server;
mod upstream;

pub use config::{
    ConfigError, DEFAULT_PORT, DEFAULT_SECTION, Fallback, PingoraConfig, Resolved, Route, Selection,
};
pub use error::PingoraError;
pub use lifecycle::{Lifecycle, LifecycleCell, LifecycleEvent};
pub use plugin::{PLUGIN_NAME, PingoraPlugin, SUPPORTED_AUTUMN_WEB};
pub use server::PingoraHandle;
