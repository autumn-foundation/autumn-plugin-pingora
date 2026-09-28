//! Errors that stop the proxy from starting.

use std::net::SocketAddr;

use crate::config::ConfigError;

/// A startup failure. Boot stops. The plugin never falls back silently.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PingoraError {
    /// The configuration is not valid.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The listener cannot bind.
    #[error("cannot bind the proxy listener on {addr}: {source}")]
    Bind {
        /// The address.
        addr: SocketAddr,
        /// The cause.
        source: std::io::Error,
    },
    /// An upstream address does not resolve.
    #[error("route `{route}`: cannot resolve upstream \"{upstream}\": {reason}")]
    Resolve {
        /// The route name.
        route: String,
        /// The upstream as configured.
        upstream: String,
        /// The cause.
        reason: String,
    },
    /// The Autumn app address does not resolve.
    #[error("cannot resolve the Autumn app address \"{0}\" for `fallback = \"app\"`")]
    AppAddress(String),
    /// The fallback target serves TLS. v0.1 has no upstream TLS.
    #[error(
        "`fallback = \"app\"` needs a plain HTTP app, but `[server.tls]` is set; terminate TLS in front of the proxy or set `fallback = \"none\"`"
    )]
    FallbackTls,
    /// The proxy would send unmatched requests to itself.
    #[error(
        "the proxy listens on the Autumn app address {0}; `fallback = \"app\"` would loop. Use another `bind`"
    )]
    FallbackLoop(SocketAddr),
    /// The startup hook ran twice.
    #[error("the proxy has already started")]
    AlreadyStarted,
}
