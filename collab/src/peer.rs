//! WebRTC helpers around GStreamer's `webrtcbin` element.
//!
//! Connecting two peers ("negotiation") goes:
//!   host:  create OFFER  → set as local → gather addresses → invite code
//!   guest: set OFFER as remote → create ANSWER → set as local → gather → reply code
//!   host:  set ANSWER as remote → direct connection opens
//!
//! `webrtcbin` is controlled by "signals": named actions we trigger with `emit_by_name`,
//! and named events it reports, which we listen to with `connect`.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gst::prelude::*;
use gstreamer as gst;
use gstreamer_sdp as gst_sdp;
use gstreamer_webrtc as gst_webrtc;

use crate::code::Code;
use crate::events::{ConnState, Event, EventSink};

/// Public "what's my address?" server. Only sees IP addresses, never video.
pub const STUN_SERVER: &str = "stun://stun.l.google.com:19302";

/// How long to wait for address gathering before using what we have.
const GATHER_TIMEOUT: Duration = Duration::from_secs(8);

pub type Error = Box<dyn std::error::Error>;

/// Collects our network addresses ("ICE candidates") as webrtcbin discovers them.
/// Attach before creating the offer/answer, so none are missed.
pub struct Gatherer {
    candidates: Arc<Mutex<Vec<(u32, String)>>>,
    complete: mpsc::Receiver<()>,
}

impl Gatherer {
    pub fn attach(webrtc: &gst::Element) -> Gatherer {
        let candidates: Arc<Mutex<Vec<(u32, String)>>> = Arc::default();
        let (done_tx, done_rx) = mpsc::channel();

        let list = candidates.clone();
        // Event: "here's another address you can be reached at".
        webrtc.connect("on-ice-candidate", false, move |values| {
            let mline = values[1].get::<u32>().unwrap_or(0);
            let candidate = values[2].get::<String>().unwrap_or_default();
            if !candidate.is_empty() {
                list.lock().unwrap().push((mline, candidate));
            }
            None
        });

        // Event: the gathering state changed; we care about "complete".
        // A Mutex makes the sender shareable between threads (signals can fire on any thread).
        let done_tx = Mutex::new(done_tx);
        webrtc.connect_notify(Some("ice-gathering-state"), move |webrtc, _| {
            let state = webrtc.property::<gst_webrtc::WebRTCICEGatheringState>("ice-gathering-state");
            if state == gst_webrtc::WebRTCICEGatheringState::Complete {
                let _ = done_tx.lock().unwrap().send(());
            }
        });

        Gatherer { candidates, complete: done_rx }
    }

    /// Wait until gathering finishes (or times out), then build our code.
    pub fn into_code(self, name: &str, local_sdp: String, events: &EventSink) -> Code {
        if self.complete.recv_timeout(GATHER_TIMEOUT).is_err() {
            events(Event::Info("Address gathering is slow; continuing with what was found.".into()));
        }
        let candidates = self.candidates.lock().unwrap().clone();
        Code { name: name.to_string(), sdp: local_sdp, candidates }
    }
}

/// Trigger a webrtcbin action that answers through a "promise" and wait for the reply.
fn call_with_promise(
    webrtc: &gst::Element,
    action: &str,
    arg: Option<&gst_webrtc::WebRTCSessionDescription>,
) -> Result<Option<gst::Structure>, Error> {
    let promise = gst::Promise::new();
    match arg {
        Some(desc) => webrtc.emit_by_name::<()>(action, &[desc, &promise]),
        None => webrtc.emit_by_name::<()>(action, &[&None::<gst::Structure>, &promise]),
    }
    match promise.wait() {
        gst::PromiseResult::Replied => {}
        other => return Err(format!("{action} failed: {other:?}").into()),
    }
    let reply = promise.get_reply().map(|s| s.to_owned());
    // webrtcbin reports failures as an "error" field in the reply.
    if let Some(err) = reply.as_ref().and_then(|r| r.get::<gst::glib::Error>("error").ok()) {
        return Err(format!("{action} failed: {err}").into());
    }
    Ok(reply)
}

/// Create our offer (host) or answer (guest), set it as local, return its SDP text.
pub fn create_local(webrtc: &gst::Element, kind: gst_webrtc::WebRTCSDPType) -> Result<String, Error> {
    let (action, field) = match kind {
        gst_webrtc::WebRTCSDPType::Offer => ("create-offer", "offer"),
        _ => ("create-answer", "answer"),
    };
    let reply = call_with_promise(webrtc, action, None)?.ok_or("no reply from webrtcbin")?;
    let desc = reply.get::<gst_webrtc::WebRTCSessionDescription>(field)?;
    call_with_promise(webrtc, "set-local-description", Some(&desc))?;
    Ok(desc.sdp().as_text()?)
}

/// Apply the other side's code: their SDP as remote description, then their addresses.
pub fn apply_remote(webrtc: &gst::Element, kind: gst_webrtc::WebRTCSDPType, code: &Code) -> Result<(), Error> {
    let sdp = gst_sdp::SDPMessage::parse_buffer(code.sdp.as_bytes())
        .map_err(|_| "the code's connection details couldn't be read")?;
    let desc = gst_webrtc::WebRTCSessionDescription::new(kind, sdp);
    call_with_promise(webrtc, "set-remote-description", Some(&desc))?;
    for (mline, candidate) in &code.candidates {
        webrtc.emit_by_name::<()>("add-ice-candidate", &[mline, candidate]);
    }
    Ok(())
}

/// Report connection state changes (connecting → connected, or failed).
pub fn report_connection_state(webrtc: &gst::Element, events: EventSink) {
    webrtc.connect_notify(Some("ice-connection-state"), move |webrtc, _| {
        use gst_webrtc::WebRTCICEConnectionState as S;
        let state = match webrtc.property::<S>("ice-connection-state") {
            S::Checking => ConnState::Connecting,
            S::Connected | S::Completed => ConnState::Connected,
            S::Failed => ConnState::Failed,
            S::Disconnected => ConnState::Disconnected,
            S::Closed => ConnState::Closed,
            _ => return,
        };
        events(Event::Connection(state));
    });
}

/// Watch the pipeline's message bus for errors on a background thread.
pub fn watch_bus(pipeline: &gst::Pipeline, events: EventSink) {
    let bus = pipeline.bus().expect("pipelines always have a bus");
    std::thread::spawn(move || {
        for msg in bus.iter_timed(gst::ClockTime::NONE) {
            match msg.view() {
                gst::MessageView::Error(e) => {
                    events(Event::Warning(format!("{} ({:?})", e.error(), e.debug())));
                }
                gst::MessageView::Eos(_) => break,
                _ => {}
            }
        }
    });
}
