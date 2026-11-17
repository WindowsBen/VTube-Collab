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

/// Partners are identified by a short id (from the room server, e.g. "m3").
pub type PeerId = String;

#[derive(Debug, Clone)]
pub enum Event {
    /// A progress message, e.g. "Preparing invite code...".
    Info(String),
    /// The direct connection to one partner changed state.
    Connection { peer: PeerId, state: ConnState },
    /// A partner's video stream started arriving.
    PartnerVideo { peer: PeerId },
    /// We're publishing a partner as this Spout2 source name.
    Publishing { peer: PeerId, source: String },
    /// We're publishing the group source (everyone stacked) under this name.
    GroupPublishing(String),
    /// Every 5 s: our sending frame rate, and how each partner's video is arriving.
    Stats { sent_fps: f64, received: Vec<PartnerStats> },
    /// Something went wrong but the collab keeps going.
    Warning(String),
}

/// How one partner's connection is doing (over the last ~5 s).
#[derive(Debug, Clone)]
pub struct PartnerStats {
    pub id: PeerId,
    pub name: String,
    /// Video frames per second arriving from them.
    pub fps: f64,
    /// Share of their video packets lost on the way (before any were resent), in %.
    pub loss_percent: Option<f64>,
    /// How often a loss couldn't be repaired in time (a frame was skipped and a fresh
    /// full frame requested). 0 = resending kept up; high = raise the receive buffer.
    pub unrepaired: Option<u32>,
    /// Time for a message to go there and back, in milliseconds.
    pub round_trip_ms: Option<f64>,
}

/// A function that receives events. `Arc` = shareable between threads;
/// `Send + Sync` = safe to call from any thread.
pub type EventSink = Arc<dyn Fn(Event) + Send + Sync>;
