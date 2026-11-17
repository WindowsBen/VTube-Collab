//! collab: the connection engine of VTube Collab, shared by the app window
//! (`app/`) and the command-line tool (`collab.exe`, src/main.rs).

pub mod code;
mod connection;
mod encoder;
pub mod events;
mod peer;
pub mod rooms;
mod session;

pub use connection::Connection;
pub use events::{ConnState, Event, EventSink};
pub use peer::Error;

/// Prefix of the Spout sources we publish for partners.
pub const PARTNER_PREFIX: &str = "Collab - ";

/// Spout2 sources on this PC that could be your avatar (our own partner outputs excluded).
pub fn avatar_sources() -> Vec<String> {
    vtc_core::spout::read_sender_names()
        .into_iter()
        .filter(|n| !n.starts_with(PARTNER_PREFIX))
        .collect()
}

/// GStreamer prints some harmless internal "ERROR" lines (e.g. about empty RTCP
/// reports). Hide its log unless GST_DEBUG is set for debugging. Real problems
/// still reach us as `Event::Warning`.
pub fn quiet_gstreamer_log() {
    if std::env::var_os("GST_DEBUG").is_none() {
        gstreamer::log::set_default_threshold(gstreamer::DebugLevel::None);
    }
}
