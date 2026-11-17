//! The media side of a collab: sends our avatar to every partner and receives theirs.
//!
//!   send:    Spout texture → [GPU pack → NV12] → appsrc → H.264 encoder → h264parse → tee ─┬→ partner A
//!            (encoded ONCE, then split to each partner)                                    ├→ partner B
//!                                                                                          └→ ...
//!   per partner: queue → RTP packets → webrtcbin  ⇄ network ⇄  (their side)
//!                webrtcbin → RTP → H.264 decoder → NV12 → appsink → [GPU unpack] → Spout "Collab - <name>"
//!
//! Partners can be added and removed while running (people joining/leaving a room).
//! Each partner has their own `webrtcbin` (one direct connection per pair of people).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
// The video "prelude" includes GStreamer's general one, plus video-frame methods like plane_stride().
use gstreamer_video::prelude::*;

use vtc_core::gpu::{Nv12Layout, Packer, Unpacker};
use vtc_core::sender::{unused_name, SpoutSender};
use vtc_core::spout;

use crate::encoder;
use crate::events::{Event, EventSink, PartnerStats, PeerId};
use crate::group::{self, GroupLayer, GroupLayers};
use crate::peer::{self, Error};

pub const FPS: u64 = 60;
/// Starting bitrate, and the most any one stream gets.
const MAX_STREAM_KBPS: u32 = 8000;
/// Total upload we aim to stay under: the per-partner bitrate shrinks as the room grows.
const UPLOAD_BUDGET_KBPS: u32 = 24_000;
/// ...but never below this (it starts looking blocky).
const MIN_STREAM_KBPS: u32 = 2500;

/// How long incoming video is held before showing it, so packets that arrive late or
/// get resent after being lost can still be used. More = smoother on bad connections,
/// but your partners' avatars appear that much later. The room host can change it.
pub const DEFAULT_BUFFER_MS: u32 = 120;
pub const MIN_BUFFER_MS: u32 = 20;
pub const MAX_BUFFER_MS: u32 = 500;

/// Who starts the negotiation with a partner (makes the "offer").
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Role {
    Offerer,
    Answerer,
}

/// A decoded packed frame on its way to a partner's Spout thread: the two NV12 planes.
struct Frame {
    y: Vec<u8>,
    y_stride: usize,
    uv: Vec<u8>,
    uv_stride: usize,
    /// Avatar size (the packed frame is twice as wide).
    w: usize,
    h: usize,
}

/// Everything belonging to one partner.
struct Partner {
    name: Arc<Mutex<String>>,
    role: Role,
    webrtc: gst::Element,
    send_bin: gst::Bin,
    tee_pad: gst::Pad,
    /// The decoder chain, added once their video arrives.
    recv_bin: Arc<Mutex<Option<gst::Bin>>>,
    gatherer: Option<peer::Gatherer>,
    received: Arc<AtomicU64>,
    received_at_last_report: u64,
    /// Latest connection statistics from webrtcbin (filled in the background).
    net_stats: Arc<Mutex<Option<gst::Structure>>>,
    /// Packet counters at the last report, to work out loss over the last 5 s.
    packets_at_last_report: (i64, u64),
    /// Fresh-frame requests at the last report.
    requests_at_last_report: u32,
    spout_stop: Arc<AtomicBool>,
    spout_thread: Option<JoinHandle<Result<(), String>>>,
}

pub struct Session {
    pipeline: gst::Pipeline,
    appsrc: gst_app::AppSrc,
    tee: gst::Element,
    encoder: Option<gst::Element>,
    /// Memory layout of the packed NV12 frames we send.
    send_info: gst_video::VideoInfo,
    partners: HashMap<PeerId, Partner>,
    /// Layer order/visibility for the group source, shared with the app window.
    pub group_layers: GroupLayers,
    /// Whether to publish the group source at all. Shared with the app window's
    /// checkbox; off = the group thread is stopped and uses no resources.
    pub group_enabled: Arc<AtomicBool>,
    group_stop: Arc<AtomicBool>,
    group_thread: Option<JoinHandle<()>>,
    events: EventSink,
    /// Set while shutting down (so removing partners doesn't announce quality changes).
    closing: bool,
    /// Receive buffer for incoming video, in milliseconds.
    buffer_ms: u32,
}

impl Session {
    /// Build the sending pipeline for a `w`×`h` avatar and start it (no partners yet).
    pub fn start(w: usize, h: usize, events: EventSink) -> Result<Session, Error> {
        let enc = encoder::pick(MAX_STREAM_KBPS, FPS)?;
        events(Event::Info(format!("Encoder: {}", enc.label)));

        // We send packed NV12 frames (twice the avatar width) in BT.709 limited range,
        // exactly what the GPU pack shader produces.
        let send_info = gst_video::VideoInfo::builder(gst_video::VideoFormat::Nv12, (w * 2) as u32, h as u32)
            .fps(gst::Fraction::new(FPS as i32, 1))
            .colorimetry(&"bt709".parse()?)
            .build()?;
        let caps = send_info.to_caps()?;

        // `tee` splits the encoded video to every partner; allow-not-linked = fine with none.
        let text = format!(
            "appsrc name=src is-live=true format=time caps=\"{caps}\" \
             ! {enc} \
             ! h264parse config-interval=-1 \
             ! tee name=split allow-not-linked=true",
            enc = enc.pipeline,
        );
        let pipeline = gst::parse::launch(&text)?
            .downcast::<gst::Pipeline>()
            .map_err(|_| "not a pipeline")?;
        let appsrc = pipeline
            .by_name("src")
            .ok_or("no src")?
            .downcast::<gst_app::AppSrc>()
            .map_err(|_| "src is not an appsrc")?;
        let tee = pipeline.by_name("split").ok_or("no tee")?;
        let encoder = pipeline.by_name("enc");

        peer::watch_bus(&pipeline, events.clone());
        pipeline.set_state(gst::State::Playing)?;

        Ok(Session {
            pipeline,
            appsrc,
            tee,
            encoder,
            send_info,
            partners: HashMap::new(),
            group_layers: GroupLayers::default(),
            group_enabled: Arc::new(AtomicBool::new(false)), // off until someone wants it
            group_stop: Arc::new(AtomicBool::new(false)),
            group_thread: None,
            events,
            closing: false,
            buffer_ms: DEFAULT_BUFFER_MS,
        })
    }

    /// Set the receive buffer (clamped to 20–500 ms), for current and future partners.
    pub fn set_receive_buffer(&mut self, ms: u32) {
        self.buffer_ms = ms.clamp(MIN_BUFFER_MS, MAX_BUFFER_MS);
        for p in self.partners.values() {
            p.webrtc.set_property("latency", self.buffer_ms);
        }
    }

    // ---------------- partners ----------------

    /// Add a partner: their own send branch, connection, receiver and Spout source.
    pub fn add_partner(&mut self, id: &str, name: &str, role: Role) -> Result<(), Error> {
        if self.partners.contains_key(id) {
            return Ok(());
        }
        // Send branch: queue → RTP packets → webrtcbin. The queue gives each partner its
        // own thread. (Standard GStreamer settings: holds up to 1 s, doesn't drop data —
        // this is compressed video, and a missing piece corrupts frames until the next keyframe.)
        let send_bin = gst::parse::bin_from_description(
            "queue \
             ! rtph264pay config-interval=-1 aggregate-mode=zero-latency pt=96 \
             ! capsfilter caps=\"application/x-rtp,media=video,encoding-name=H264,payload=96,clock-rate=90000\"",
            true,
        )?;
        let webrtc = gst::ElementFactory::make("webrtcbin")
            .property_from_str("bundle-policy", "max-bundle")
            .property("stun-server", peer::STUN_SERVER)
            .property("latency", self.buffer_ms)
            .build()?;
        simulate_bad_network(&webrtc);
        self.pipeline.add_many([send_bin.upcast_ref::<gst::Element>(), &webrtc])?;
        send_bin.link(&webrtc)?;

        // Ask for lost packets to be sent again ("NACK"). Without this, one lost packet
        // spoils the video until the next full frame (up to 1 s later).
        if let Some(transceiver) =
            webrtc.emit_by_name::<Option<gstreamer_webrtc::WebRTCRTPTransceiver>>("get-transceiver", &[&0i32])
        {
            transceiver.set_property("do-nack", true);
        }

        let gatherer = peer::Gatherer::attach(&webrtc);
        peer::report_connection_state(&webrtc, id.to_string(), self.events.clone());

        let (frame_tx, frame_rx) = mpsc::sync_channel::<Frame>(2);
        let received = Arc::new(AtomicU64::new(0));
        let recv_bin = Arc::new(Mutex::new(None));
        attach_receiver(&self.pipeline, &webrtc, id, frame_tx, received.clone(), recv_bin.clone(), self.events.clone());

        let name = Arc::new(Mutex::new(name.to_string()));
        let spout_stop = Arc::new(AtomicBool::new(false));
        let spout_thread = spawn_spout_thread(
            id.to_string(),
            frame_rx,
            name.clone(),
            spout_stop.clone(),
            self.group_layers.clone(),
            self.events.clone(),
        );

        webrtc.sync_state_with_parent()?;
        send_bin.sync_state_with_parent()?;
        let tee_pad = self.tee.request_pad_simple("src_%u").ok_or("couldn't split the video")?;
        tee_pad.link(&send_bin.static_pad("sink").ok_or("no send input")?)?;

        // New partners go on top of the group source.
        let label = name.lock().unwrap().clone();
        self.group_layers.lock().unwrap().insert(
            0,
            GroupLayer { id: id.to_string(), source: group::partner_source(&label), label, visible: true, is_me: false },
        );

        self.partners.insert(
            id.to_string(),
            Partner {
                name,
                role,
                webrtc,
                send_bin,
                tee_pad,
                recv_bin,
                gatherer: Some(gatherer),
                received,
                received_at_last_report: 0,
                net_stats: Arc::default(),
                packets_at_last_report: (0, 0),
                requests_at_last_report: 0,
                spout_stop,
                spout_thread: Some(spout_thread),
            },
        );
        self.update_bitrate();
        Ok(())
    }

    /// Remove a partner (they left): disconnect, stop their Spout source, drop their layer.
    pub fn remove_partner(&mut self, id: &str) {
        let Some(mut p) = self.partners.remove(id) else { return };
        p.spout_stop.store(true, Ordering::Relaxed);
        if let Some(t) = p.spout_thread.take() {
            let _ = t.join();
        }
        if let Some(sink) = p.send_bin.static_pad("sink") {
            let _ = p.tee_pad.unlink(&sink);
        }
        self.tee.release_request_pad(&p.tee_pad);
        let mut elements: Vec<gst::Element> = vec![p.webrtc.clone(), p.send_bin.clone().upcast()];
        if let Some(bin) = p.recv_bin.lock().unwrap().take() {
            elements.push(bin.upcast());
        }
        for e in &elements {
            let _ = e.set_state(gst::State::Null);
            let _ = self.pipeline.remove(e);
        }
        self.group_layers.lock().unwrap().retain(|l| l.id != id);
        self.update_bitrate();
    }

    pub fn partner_role(&self, id: &str) -> Option<Role> {
        self.partners.get(id).map(|p| p.role)
    }

    pub fn partner_name(&self, id: &str) -> Option<String> {
        self.partners.get(id).map(|p| p.name.lock().unwrap().clone())
    }

    /// The partner's connection element (for negotiation).
    pub fn partner_webrtc(&self, id: &str) -> Option<gst::Element> {
        self.partners.get(id).map(|p| p.webrtc.clone())
    }

    /// The partner's address collector; can be taken once, when building our code for them.
    pub fn take_gatherer(&mut self, id: &str) -> Option<peer::Gatherer> {
        self.partners.get_mut(id).and_then(|p| p.gatherer.take())
    }

    /// Rename a partner (e.g. once their reply code tells us their name).
    pub fn rename_partner(&mut self, id: &str, name: &str) {
        if let Some(p) = self.partners.get(id) {
            *p.name.lock().unwrap() = name.to_string();
        }
        for layer in self.group_layers.lock().unwrap().iter_mut().filter(|l| l.id == id) {
            layer.label = name.to_string();
            layer.source = group::partner_source(name);
        }
    }

    /// Spread the upload budget over the partners (we send one stream to each).
    fn update_bitrate(&self) {
        let n = self.partners.len().max(1) as u32;
        let kbps = (UPLOAD_BUDGET_KBPS / n).clamp(MIN_STREAM_KBPS, MAX_STREAM_KBPS);
        if let Some(enc) = &self.encoder {
            if enc.find_property("bitrate").is_some() {
                enc.set_property_from_str("bitrate", &kbps.to_string());
            }
        }
        if !self.partners.is_empty() && !self.closing {
            let total = kbps as f64 * self.partners.len() as f64 / 1000.0;
            self.events.clone()(Event::Info(format!(
                "Quality: {:.1} Mbit/s per partner, {total:.1} Mbit/s upload in total.",
                kbps as f64 / 1000.0
            )));
        }
    }

    // ---------------- running ----------------

    /// Capture and send until `stop` is set, then shut down cleanly. `tick` runs every
    /// frame (≈60×/s) so the caller can add/remove partners as people join and leave.
    pub fn run(
        mut self,
        sender_name: &str,
        my_name: &str,
        packer: Packer,
        stop: &AtomicBool,
        mut tick: impl FnMut(&mut Session) -> Result<(), Error>,
    ) -> Result<(), Error> {
        // Your own avatar: bottom layer of the group source.
        self.group_layers.lock().unwrap().push(GroupLayer {
            id: group::ME.into(),
            label: format!("You ({my_name})"),
            source: sender_name.to_string(),
            visible: true,
            is_me: true,
        });

        let result = self.send_loop(sender_name, packer, stop, &mut tick);
        self.shutdown();
        result
    }

    fn send_loop(
        &mut self,
        sender_name: &str,
        mut packer: Packer,
        stop: &AtomicBool,
        tick: &mut impl FnMut(&mut Session) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let (w, h) = (packer.width, packer.height);
        (self.events)(Event::Info(format!("Sending \"{sender_name}\" ({w}×{h}).")));

        let layout = Nv12Layout {
            y_offset: self.send_info.offset()[0],
            y_stride: self.send_info.stride()[0] as usize,
            uv_offset: self.send_info.offset()[1],
            uv_stride: self.send_info.stride()[1] as usize,
        };
        let frame_size = self.send_info.size();

        let frame_ns = 1_000_000_000 / FPS;
        let frame_interval = Duration::from_nanos(frame_ns);
        let start = Instant::now();
        let (mut sent, mut last_sent) = (0u64, 0u64);
        let mut last_report = Instant::now();

        let mut i: u64 = 0;
        while !stop.load(Ordering::Relaxed) {
            tick(self)?;
            self.sync_group_source();

            // If the avatar app restarted, reopen it (size must stay the same for now).
            if i % FPS == 0 {
                if let Some(now) = spout::read_sender_info(sender_name) {
                    if now.share_handle != packer.share_handle {
                        let p = Packer::open(&now)?;
                        if (p.width, p.height) != (w, h) {
                            return Err("the avatar's resolution changed; restart the collab".into());
                        }
                        packer = p;
                    }
                }
            }

            // Only capture and encode when someone is there to receive it.
            if !self.partners.is_empty() {
                let mut buffer = gst::Buffer::with_size(frame_size)?;
                {
                    let b = buffer.get_mut().ok_or("buffer not writable")?;
                    {
                        let mut map = b.map_writable()?;
                        packer.pack_nv12(&mut map, layout)?;
                    }
                    b.set_pts(gst::ClockTime::from_nseconds(i * frame_ns));
                    b.set_duration(gst::ClockTime::from_nseconds(frame_ns));
                }
                self.appsrc.push_buffer(buffer)?;
                sent += 1;
            }

            let since = last_report.elapsed();
            if since >= Duration::from_secs(5) {
                let secs = since.as_secs_f64();
                let mut received = Vec::new();
                for (id, p) in self.partners.iter_mut() {
                    let now = p.received.load(Ordering::Relaxed);
                    let net = network_numbers(p);
                    received.push(PartnerStats {
                        id: id.clone(),
                        name: p.name.lock().unwrap().clone(),
                        fps: (now - p.received_at_last_report) as f64 / secs,
                        loss_percent: net.loss_percent,
                        unrepaired: net.unrepaired,
                        round_trip_ms: net.round_trip_ms,
                    });
                    p.received_at_last_report = now;
                    request_net_stats(&p.webrtc, p.net_stats.clone()); // ready for next time
                }
                received.sort_by(|a, b| a.name.cmp(&b.name));
                (self.events)(Event::Stats { sent_fps: (sent - last_sent) as f64 / secs, received });
                (last_sent, last_report) = (sent, Instant::now());
            }

            i += 1;
            let next_due = start + frame_interval * i as u32;
            let now = Instant::now();
            if next_due > now {
                std::thread::sleep(next_due - now);
            }
        }
        Ok(())
    }

    /// Start or stop the group source to match the `group_enabled` switch.
    fn sync_group_source(&mut self) {
        let wanted = self.group_enabled.load(Ordering::Relaxed);
        if wanted && self.group_thread.is_none() {
            self.group_stop = Arc::new(AtomicBool::new(false));
            self.group_thread = Some(group::spawn(self.group_layers.clone(), self.group_stop.clone(), self.events.clone()));
        } else if !wanted && self.group_thread.is_some() {
            self.stop_group_source();
            (self.events)(Event::GroupStopped);
        }
    }

    /// Stop the group thread (this also removes "Collab - Group" from OBS's list).
    fn stop_group_source(&mut self) {
        self.group_stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.group_thread.take() {
            let _ = t.join();
        }
    }

    fn shutdown(&mut self) {
        self.closing = true;
        // Group source first (it reads the partners' sources), then each partner.
        self.stop_group_source();
        let ids: Vec<PeerId> = self.partners.keys().cloned().collect();
        for id in ids {
            self.remove_partner(&id);
        }
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

impl Drop for Session {
    /// If a session is abandoned before `run` (e.g. "Cancel" while waiting), still stop
    /// the pipeline and remove the Spout sources. Running this twice is harmless.
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// TEST ONLY — imitate a bad internet route on one PC. Never set in normal use.
///   VTC_SIMULATE_LOSS=0.03       drop 3% of the packets we send
///   VTC_SIMULATE_DELAY_MS=60     delay every packet we send by 60 ms
///
/// The simulator goes where webrtcbin hands packets to the network ("aux sender"), i.e.
/// *after* webrtcbin keeps a copy for resending — like real internet loss, so lost
/// packets can still be resent (and arrive late, by the delay).
fn simulate_bad_network(webrtc: &gst::Element) {
    let var = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<f64>().ok());
    let loss = var("VTC_SIMULATE_LOSS").filter(|p| *p > 0.0 && *p < 1.0);
    let delay = var("VTC_SIMULATE_DELAY_MS").filter(|d| *d > 0.0).map(|d| d as i32);
    if loss.is_none() && delay.is_none() {
        return;
    }
    webrtc.connect("request-aux-sender", false, move |_| {
        let mut builder = gst::ElementFactory::make("netsim");
        if let Some(p) = loss {
            builder = builder.property("drop-probability", p as f32);
        }
        if let Some(ms) = delay {
            builder = builder
                .property("delay-probability", 1.0f32)
                .property("min-delay", ms)
                .property("max-delay", ms);
        }
        builder.build().ok().map(|e| e.to_value())
    });
}

/// Ask webrtcbin for its connection statistics. The answer arrives in the background
/// and is read at the next 5-second report (so the video loop never waits for it).
fn request_net_stats(webrtc: &gst::Element, out: Arc<Mutex<Option<gst::Structure>>>) {
    let promise = gst::Promise::with_change_func(move |reply| {
        if let Ok(Some(stats)) = reply {
            *out.lock().unwrap() = Some(stats.to_owned());
        }
    });
    webrtc.emit_by_name::<()>("get-stats", &[&None::<gst::Pad>, &promise]);
}

/// From the latest statistics: packet loss %, unrepaired losses (since last report),
/// and round-trip time.
///
/// The statistics are a list of entries, each with a "type":
///   * "inbound-rtp": video coming from the partner — packets received / lost (running totals)
///   * "remote-inbound-rtp": what the partner reports about our video — includes round-trip time
fn network_numbers(p: &mut Partner) -> NetNumbers {
    use gstreamer_webrtc::WebRTCStatsType;
    let Some(stats) = p.net_stats.lock().unwrap().clone() else { return NetNumbers::default() };

    let (mut lost, mut received, mut requests, mut rtt) = (None::<i64>, None::<u64>, None::<u32>, None::<f64>);
    for (_, value) in stats.iter() {
        let Ok(entry) = value.get::<gst::Structure>() else { continue };
        match entry.get::<WebRTCStatsType>("type") {
            Ok(WebRTCStatsType::InboundRtp) => {
                // The raw network counters live in the nested "gst-rtpsource-stats"
                // (the top-level "packets-lost" stays 0 because resent packets count as received).
                if let Ok(source) = entry.get::<gst::Structure>("gst-rtpsource-stats") {
                    lost = source.get::<i32>("packets-lost").ok().map(i64::from).or(lost);
                    received = source.get::<u64>("packets-received").ok().or(received);
                }
                // Each time we asked for a fresh full frame, a loss couldn't be repaired in time.
                let fir = entry.get::<u32>("fir-count").unwrap_or(0);
                let pli = entry.get::<u32>("pli-count").unwrap_or(0);
                requests = Some(fir + pli);
            }
            Ok(WebRTCStatsType::RemoteInboundRtp) => {
                rtt = entry.get::<f64>("round-trip-time").ok().filter(|t| *t > 0.0).or(rtt);
            }
            _ => {}
        }
    }

    let mut out = NetNumbers { round_trip_ms: rtt.map(|seconds| seconds * 1000.0), ..Default::default() };
    if let (Some(lost), Some(received)) = (lost, received) {
        let (prev_lost, prev_received) = p.packets_at_last_report;
        p.packets_at_last_report = (lost, received);
        let new_lost = (lost - prev_lost).max(0) as f64;
        let new_received = received.saturating_sub(prev_received) as f64;
        out.loss_percent = (new_lost + new_received > 0.0).then(|| 100.0 * new_lost / (new_lost + new_received));
    }
    if let Some(requests) = requests {
        out.unrepaired = Some(requests.saturating_sub(p.requests_at_last_report));
        p.requests_at_last_report = requests;
    }
    out
}

/// Connection numbers for one partner over the last report period.
#[derive(Default)]
struct NetNumbers {
    loss_percent: Option<f64>,
    round_trip_ms: Option<f64>,
    unrepaired: Option<u32>,
}

/// When a partner's video stream appears, attach: depacketiser → decoder → appsink.
fn attach_receiver(
    pipeline: &gst::Pipeline,
    webrtc: &gst::Element,
    peer_id: &str,
    frame_tx: mpsc::SyncSender<Frame>,
    received: Arc<AtomicU64>,
    recv_bin: Arc<Mutex<Option<gst::Bin>>>,
    events: EventSink,
) {
    let pipeline_weak = pipeline.downgrade(); // "weak" = doesn't keep the pipeline alive
    let peer_id = peer_id.to_string();
    webrtc.connect_pad_added(move |_, pad| {
        let Some(pipeline) = pipeline_weak.upgrade() else { return };
        if pad.direction() != gst::PadDirection::Src {
            return;
        }
        // If video packets go missing (and resending them didn't arrive in time), don't
        // show damaged frames — they smear until the next full frame. Instead skip ahead
        // to the next full frame ("keyframe") and ask the partner to send one right away.
        //
        // d3d11h264dec decodes on any graphics card (NVIDIA, AMD or Intel) and hands
        // us NV12 in normal memory; the GPU unpack shader does the rest.
        let bin = match gst::parse::bin_from_description(
            "rtph264depay request-keyframe=true wait-for-keyframe=true \
             ! h264parse ! d3d11h264dec ! video/x-raw,format=NV12 \
             ! appsink name=sink sync=false max-buffers=2 drop=true",
            true,
        ) {
            Ok(b) => b,
            Err(e) => return events(Event::Warning(format!("couldn't build decoder: {e}"))),
        };
        let appsink = bin
            .by_name("sink")
            .and_then(|s| s.downcast::<gst_app::AppSink>().ok())
            .expect("sink is an appsink");

        let frame_tx = frame_tx.clone();
        let received = received.clone();
        appsink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                    let caps = sample.caps().ok_or(gst::FlowError::Error)?;
                    let info = gst_video::VideoInfo::from_caps(caps).map_err(|_| gst::FlowError::Error)?;
                    // A "video frame" view knows where each plane starts and its row stride.
                    let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info)
                        .map_err(|_| gst::FlowError::Error)?;
                    let msg = Frame {
                        y: frame.plane_data(0).map_err(|_| gst::FlowError::Error)?.to_vec(),
                        y_stride: frame.plane_stride()[0] as usize,
                        uv: frame.plane_data(1).map_err(|_| gst::FlowError::Error)?.to_vec(),
                        uv_stride: frame.plane_stride()[1] as usize,
                        w: info.width() as usize / 2,
                        h: info.height() as usize,
                    };
                    let _ = frame_tx.try_send(msg);
                    received.fetch_add(1, Ordering::Relaxed);
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        if pipeline.add(&bin).is_err() || bin.sync_state_with_parent().is_err() {
            return events(Event::Warning("couldn't start decoder".into()));
        }
        let sink_pad = bin.static_pad("sink").expect("bin has a sink pad");
        match pad.link(&sink_pad) {
            Ok(_) => {
                *recv_bin.lock().unwrap() = Some(bin);
                events(Event::PartnerVideo { peer: peer_id.clone() });
            }
            Err(e) => events(Event::Warning(format!("couldn't connect incoming video: {e:?}"))),
        }
    });
}

/// Unpacks one partner's frames on the GPU, straight into Spout2 "Collab - <name>".
/// The Spout source is created on the first frame (that's when we know the size).
fn spawn_spout_thread(
    peer_id: PeerId,
    frame_rx: mpsc::Receiver<Frame>,
    name: Arc<Mutex<String>>,
    stop: Arc<AtomicBool>,
    layers: GroupLayers,
    events: EventSink,
) -> JoinHandle<Result<(), String>> {
    std::thread::spawn(move || {
        // (sender, unpacker) — created together, on the same GPU device.
        let mut out: Option<(SpoutSender, Unpacker)> = None;
        // Check the stop flag at least every 100 ms, so we always exit when asked.
        while !stop.load(Ordering::Relaxed) {
            let f = match frame_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(frame) => frame,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            if out.is_none() {
                // Two partners with the same name get "Collab - X" and "Collab - X 2".
                let source = unused_name(&group::partner_source(&name.lock().unwrap()));
                let sender = SpoutSender::new(&source, f.w as u32, f.h as u32).map_err(|e| e.to_string())?;
                let unpacker = Unpacker::new(sender.device(), f.w as u32, f.h as u32).map_err(|e| e.to_string())?;
                for layer in layers.lock().unwrap().iter_mut().filter(|l| l.id == peer_id) {
                    layer.source = source.clone();
                }
                events(Event::Publishing { peer: peer_id.clone(), source });
                out = Some((sender, unpacker));
            }
            let (sender, unpacker) = out.as_mut().unwrap();
            sender
                .render(|ctx, target| unpacker.unpack(ctx, target, &f.y, f.y_stride, &f.uv, f.uv_stride))
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    })
}
