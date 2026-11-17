//! The group source "Collab - Group": everyone in the collab stacked into one
//! Spout source, in a layer order the user controls (like layers in a photo editor).
//!
//! The layer list is shared between the connection and the app window: the window
//! reorders / hides layers, and the group thread picks the changes up next frame.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vtc_core::compositor::Compositor;

use crate::events::{Event, EventSink};

pub const GROUP_SOURCE: &str = "Collab - Group";
const CANVAS: (u32, u32) = (1920, 1080);
const FPS: u32 = 60;

#[derive(Clone, Debug)]
pub struct GroupLayer {
    /// Which partner this is (their peer id), or `ME` for your own avatar.
    pub id: String,
    /// What the window shows, e.g. "You (Ben)" or "Beat".
    pub label: String,
    /// The Spout source this layer shows.
    pub source: String,
    pub visible: bool,
    pub is_me: bool,
}

/// The shared layer list. **Index 0 is the top layer** (drawn last, in front).
pub type GroupLayers = Arc<Mutex<Vec<GroupLayer>>>;

/// The Spout source name for a partner's avatar ("Collab - <name>"), kept
/// different from the group source even if someone is literally called "Group".
pub fn partner_source(partner: &str) -> String {
    let name = format!("{}{partner}", crate::PARTNER_PREFIX);
    if name.eq_ignore_ascii_case(GROUP_SOURCE) { format!("{name} (partner)") } else { name }
}

/// Layer id used for your own avatar.
pub const ME: &str = "me";

/// Draw the group source at 60 fps until `stop` is set.
pub fn spawn(layers: GroupLayers, stop: Arc<AtomicBool>, events: EventSink) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut compositor = match Compositor::new(GROUP_SOURCE, CANVAS.0, CANVAS.1) {
            Ok(c) => c,
            Err(e) => return events(Event::Warning(format!("group source unavailable: {e}"))),
        };
        events(Event::GroupPublishing(compositor.name().to_string()));

        let interval = Duration::from_secs(1) / FPS;
        let mut next = Instant::now();
        while !stop.load(Ordering::Relaxed) {
            // Visible layers, bottom first (the list itself is top first).
            let sources: Vec<String> = layers
                .lock()
                .unwrap()
                .iter()
                .rev()
                .filter(|l| l.visible)
                .map(|l| l.source.clone())
                .collect();
            if let Err(e) = compositor.render(&sources) {
                events(Event::Warning(format!("group source: {e}")));
                std::thread::sleep(Duration::from_millis(500)); // don't spam warnings
            }
            next += interval;
            let now = Instant::now();
            if next > now {
                std::thread::sleep(next - now);
            } else {
                next = now; // fell behind; don't try to catch up
            }
        }
        // Dropping the compositor removes "Collab - Group" from Spout's list.
    })
}
