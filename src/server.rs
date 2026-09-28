//! The accept loop, the drain and [`PingoraHandle`].

use std::net::SocketAddr;
use std::sync::Arc;

use crate::lifecycle::{Lifecycle, LifecycleCell};

/// State shared by the plugin, the server and the handles.
#[derive(Debug, Default)]
pub struct Shared {
    pub lifecycle: LifecycleCell,
}

/// A handle to the proxy. Clone it freely.
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
    pub const fn local_addr(&self) -> Option<SocketAddr> {
        None
    }

    /// Open client connections.
    #[must_use]
    pub const fn active_connections(&self) -> usize {
        0
    }

    /// `(route, healthy, total)` upstream counts, in route order.
    #[must_use]
    pub const fn upstream_health(&self) -> Vec<(String, usize, usize)> {
        Vec::new()
    }

    /// Stop the proxy and wait for the drain.
    pub async fn shutdown(&self) {}
}
