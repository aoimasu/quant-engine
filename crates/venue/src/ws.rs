//! The websocket transport seam.
//!
//! Pull-based ([`WsConnection::poll`]) so the registry is single-threaded and deterministic in tests — no
//! real socket, no threads in the tested core. The real `tungstenite` adapter lives behind the `http`
//! feature; tests drive a scripted fake.

use thiserror::Error;

use crate::stream::{StreamMessage, StreamTier, Subscription};

/// A websocket transport failure.
#[derive(Debug, Error)]
pub enum WsError {
    /// Establishing the connection failed.
    #[error("ws connect error: {0}")]
    Connect(String),
    /// Sending a subscribe frame failed.
    #[error("ws subscribe error: {0}")]
    Subscribe(String),
    /// The connector could not (re)establish the tier socket.
    #[error("ws closed")]
    Closed,
}

/// The outcome of polling a connection once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsPoll {
    /// A decoded stream update.
    Message(StreamMessage),
    /// The socket dropped — the registry must reconnect + resubscribe.
    Disconnected,
    /// No data available right now (live sockets); the registry simply polls again later.
    Idle,
}

/// One tier's websocket connection. The single network seam.
pub trait WsConnection {
    /// (Re)subscribe to `subs` on this connection. Called on first connect and again after a reconnect.
    ///
    /// # Errors
    /// [`WsError::Subscribe`] if the subscribe frame cannot be sent.
    fn subscribe(&mut self, subs: &[Subscription]) -> Result<(), WsError>;

    /// Poll the next event (message / disconnect / idle).
    fn poll(&mut self) -> WsPoll;
}

/// Establishes a tier's websocket connection. The registry calls this to connect and to reconnect.
pub trait WsConnector {
    /// Connect a fresh socket for `tier`.
    ///
    /// # Errors
    /// [`WsError::Connect`] on failure.
    fn connect(&self, tier: StreamTier) -> Result<Box<dyn WsConnection>, WsError>;
}
