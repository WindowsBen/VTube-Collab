//! What a running connection reports back: progress, connection state, frame rates.
//!
//! The connection code doesn't print anything itself. It calls an "event sink"
//! (a function we hand it), so the same code can feed the command line (prints
//! lines) or the app window (updates labels).

use std::sync::Arc;

#[derive(Debug, Clone, PartialEq)]
pub enum ConnState {
    Connecting,
    Connected,
    /// No direct route between the two PCs (strict network or firewall).
    Failed,
    Disconnected,
    Closed,
}

#[derive(Debug, Clone)]
pub enum Event {
    /// A progress message, e.g. "Preparing invite code...".
    Info(String),
    Connection(ConnState),
    /// The partner's video stream started arriving.
    PartnerVideo,
    /// We're publishing the partner as this Spout2 source name.
    Publishing(String),
    Stats { sent_fps: f64, received_fps: f64 },
    /// Something went wrong but the connection keeps going.
    Warning(String),
}

/// A function that receives events. `Arc` = shareable between threads;
/// `Send + Sync` = safe to call from any thread.
pub type EventSink = Arc<dyn Fn(Event) + Send + Sync>;
