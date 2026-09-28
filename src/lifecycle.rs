//! The proxy lifecycle as a finite state machine.

use std::fmt;
use std::sync::atomic::{AtomicU8, Ordering};

/// The state of the proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Lifecycle {
    /// Not started yet.
    Idle,
    /// The listener is bound and accepts connections.
    Serving,
    /// Shutdown started. No new connections. Open requests finish.
    Draining,
    /// The proxy stopped. Terminal.
    Stopped,
    /// The proxy could not start, or stopped unexpectedly. Terminal.
    Failed,
}

/// An input to the state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LifecycleEvent {
    /// The listener is bound.
    Bound,
    /// The proxy could not start.
    StartFailed,
    /// The app asked the proxy to stop.
    ShutdownRequested,
    /// All connections ended, or the grace period expired.
    Drained,
    /// The accept loop ended.
    ServerExited,
}

impl Lifecycle {
    /// All states.
    pub const ALL: [Self; 5] = [
        Self::Idle,
        Self::Serving,
        Self::Draining,
        Self::Stopped,
        Self::Failed,
    ];

    /// The next state after `event`, or `None` if `event` is illegal here.
    #[must_use]
    pub const fn next(self, _event: LifecycleEvent) -> Option<Self> {
        None
    }

    /// `true` only in [`Serving`](Self::Serving).
    #[must_use]
    pub const fn is_ready(self) -> bool {
        false
    }

    /// `true` in [`Stopped`](Self::Stopped) and [`Failed`](Self::Failed).
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        false
    }

    /// Lower-case name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        ""
    }

    const fn to_u8(self) -> u8 {
        match self {
            Self::Idle => 0,
            Self::Serving => 1,
            Self::Draining => 2,
            Self::Stopped => 3,
            Self::Failed => 4,
        }
    }

    const fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Idle,
            1 => Self::Serving,
            2 => Self::Draining,
            3 => Self::Stopped,
            _ => Self::Failed,
        }
    }
}

impl LifecycleEvent {
    /// All events.
    pub const ALL: [Self; 5] = [
        Self::Bound,
        Self::StartFailed,
        Self::ShutdownRequested,
        Self::Drained,
        Self::ServerExited,
    ];
}

impl fmt::Display for Lifecycle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A shared, atomic [`Lifecycle`]. The only way to change the state.
#[derive(Debug)]
pub struct LifecycleCell(AtomicU8);

impl Default for LifecycleCell {
    fn default() -> Self {
        Self::new()
    }
}

impl LifecycleCell {
    /// A cell in [`Lifecycle::Idle`].
    #[must_use]
    pub const fn new() -> Self {
        Self(AtomicU8::new(Lifecycle::Idle.to_u8()))
    }

    /// The current state.
    #[must_use]
    pub fn get(&self) -> Lifecycle {
        Lifecycle::from_u8(self.0.load(Ordering::Acquire))
    }

    /// Apply `event` atomically.
    ///
    /// # Errors
    ///
    /// Returns the unchanged state when `event` is illegal.
    pub fn apply(&self, _event: LifecycleEvent) -> Result<Lifecycle, Lifecycle> {
        Err(self.get())
    }
}
