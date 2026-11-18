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
/// Total upload we aim to stay under: the per-partner bitrate shrinks as the room grows.
const UPLOAD_BUDGET_KBPS: u32 = 24_000;

/// The size each avatar is sent at, from the room's "max people" setting. Bigger
/// rooms send smaller video: every person decodes everyone else's stream, so 7
/// streams at 1080p would overload many PCs (720p is 2.25× less work, 540p 4×).
pub fn transmit_size_for(max_people: u32) -> (u32, u32) {
    match max_people {
        0..=4 => (1920, 1080),
        5..=8 => (1280, 720),
        _ => (960, 540),
    }
}

/// The size we actually send at: the room's size, but never bigger than needed for
/// the avatar's own image. A 1266×662 window in a 1080p room is sent at 1266×712
/// (same 16:9 shape as everyone else's), not stretched up to 1920×1080 — stretching
/// adds no detail, only work for everyone's decoder. Receivers stretch it back to
/// the room's size, so their OBS sources keep the size they expect.
fn fit_transmit(room: (u32, u32), source: (u32, u32)) -> (u32, u32) {
    let (rw, rh) = (room.0 as f64, room.1 as f64);
    let fits = (rw / source.0.max(1) as f64).min(rh / source.1.max(1) as f64);
    if fits <= 1.0 {
        return room; // the source is at least as big: scale it down to the room's size
    }
    // Shrink the room's frame until the source fills it, but not below 640 wide.
    let scale = (1.0 / fits).max(640.0 / rw).min(1.0);
    ((rw * scale).round() as u32 & !1, (rh * scale).round() as u32 & !1)
}

/// Bitrate range (min, max kbps) for one stream at this transmit size.
fn bitrate_range(transmit: (u32, u32)) -> (u32, u32) {
    match transmit.0 * transmit.1 {
        1_500_000.. => (2500, 8000), // around 1080p
        800_000.. => (1800, 4500),   // around 720p
        _ => (1200, 2800),           // smaller
    }
}

/// A partner who sent no video for this long is treated as gone (their app or
/// connection died without the room server noticing).
const PARTNER_TIMEOUT: Duration = Duration::from_secs(30);
/// ...and one whose video never started at all, after this long.
const FIRST_VIDEO_TIMEOUT: Duration = Duration::from_secs(60);

/// How long incoming video is held before showing it, so packets that arrive late or
/// get resent after being lost can still be used. More = smoother on bad connections,
/// but your partners' avatars appear that much later. The room host can change it.
pub const DEFAULT_BUFFER_MS: u32 = 120;
pub const MIN_BUFFER_MS: u32 = 20;
pub const MAX_BUFFER_MS: u32 = 500;

/// Lowering quality when partners lose packets of our video (usually a sign our
/// upload is overloaded). One encoder feeds everyone, so it's the worst partner's
/// loss that counts. Checked every 5 s report:
///   loss >= LOSS_HIGH in 2 reports running -> quality x 0.8 (not below QUALITY_MIN)
///   loss < LOSS_LOW for 6 reports (30 s)   -> quality x 1.15 (back up to full)
const LOSS_HIGH_PERCENT: f64 = 4.0;
const LOSS_LOW_PERCENT: f64 = 1.0;
const QUALITY_MIN: f64 = 0.5;

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
    /// When they were added, and when their video last arrived (for spotting dead partners).
    added_at: Instant,
    last_video_at: Option<Instant>,
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
    /// The room's video size (see `transmit_size_for`). Partners' sources are this size.
    room_size: (u32, u32),
    /// Our avatar's own image size.
    source_size: (u32, u32),
    /// The size our avatar is actually sent at (see `fit_transmit`).
    transmit: (u32, u32),
    /// Set once the first frame is sent; the transmit size can't change after that.
    sent_any: bool,
    /// The video decoder to use (on the same graphics card as the avatar).
    decoder: String,
    /// Partners whose video stopped (see `take_lost_partners`).
    lost: Vec<PeerId>,
    /// Relay server addresses (from the room server), added to every new connection.
    relays: Vec<String>,
    /// Share of the normal bitrate we send at (QUALITY_MIN-1.0), lowered when partners
    /// lose packets; and how many reports in a row were bad / good.
    quality: f64,
    loss_reports: (u32, u32),
}

/// Memory layout of the packed NV12 frames we send for a `transmit`-sized avatar:
/// twice as wide (colour + alpha), BT.709 limited range — what the GPU pack shader makes.
fn packed_video_info(transmit: (u32, u32)) -> Result<gst_video::VideoInfo, Error> {
    Ok(gst_video::VideoInfo::builder(gst_video::VideoFormat::Nv12, transmit.0 * 2, transmit.1)
        .fps(gst::Fraction::new(FPS as i32, 1))
        .colorimetry(&"bt709".parse()?)
        .build()?)
}

impl Session {
    /// Build the sending pipeline for a room whose video size is `room`, for an
    /// avatar image of `source` size, and start it (no partners yet).
    pub fn start(room: (u32, u32), source: (u32, u32), events: EventSink) -> Result<Session, Error> {
        let transmit = fit_transmit(room, source);
        let enc = encoder::pick(bitrate_range(transmit).1, FPS)?;
        events(Event::Info(format!("Encoder: {}", enc.label)));
        let (decoder, decoder_label) = pick_decoder();
        events(Event::Info(format!("Decoder: {decoder_label}")));

        let send_info = packed_video_info(transmit)?;
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
            room_size: room,
            source_size: source,
            transmit,
            sent_any: false,
            decoder,
            lost: Vec::new(),
            relays: Vec::new(),
            quality: 1.0,
            loss_reports: (0, 0),
        })
    }

    /// Change the room's video size. Only possible before anything has been sent
    /// (e.g. when joining, once the room's settings arrive).
    pub fn set_transmit_size(&mut self, room: (u32, u32)) -> Result<(), Error> {
        if self.sent_any {
            return Ok(());
        }
        self.room_size = room;
        self.refit()
    }

    /// Work out the send size again (room size or source size changed). Only before
    /// the first frame is sent: after that, partners' decoders expect a fixed size.
    fn refit(&mut self) -> Result<(), Error> {
        let transmit = fit_transmit(self.room_size, self.source_size);
        if self.sent_any || transmit == self.transmit {
            return Ok(());
        }
        self.send_info = packed_video_info(transmit)?;
        self.appsrc.set_caps(Some(&self.send_info.to_caps()?));
        self.transmit = transmit;
        if !self.partners.is_empty() {
            self.update_bitrate();
        }
        Ok(())
    }

    /// Partners whose video stopped arriving (see `PARTNER_TIMEOUT`). The caller
    /// removes them; each is reported once.
    pub fn take_lost_partners(&mut self) -> Vec<PeerId> {
        std::mem::take(&mut self.lost)
    }

    /// Set the receive buffer (clamped to 20–500 ms), for current and future partners.
    /// Works while running: the host can change it for everyone mid-collab.
    pub fn set_receive_buffer(&mut self, ms: u32) {
        self.buffer_ms = ms.clamp(MIN_BUFFER_MS, MAX_BUFFER_MS);
        let running: usize = self.partners.values().map(|p| set_latency(&p.webrtc, self.buffer_ms)).sum();
        if running > 0 {
            crate::log::info(&format!("Receive buffer: {} ms ({running} running buffers changed)", self.buffer_ms));
        }
    }

    /// Relay server addresses (see `peer::relay_uris`) for connections made from now on.
    pub fn set_relays(&mut self, relays: Vec<String>) {
        self.relays = relays;
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
        for relay in &self.relays {
            webrtc.emit_by_name::<bool>("add-turn-server", &[relay]);
        }
        // TEST ONLY: VTC_RELAY_ONLY=1 forces every connection through the relay (to test it).
        if std::env::var("VTC_RELAY_ONLY").is_ok_and(|v| v == "1") {
            webrtc.set_property_from_str("ice-transport-policy", "relay");
        }
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
        attach_receiver(
            &self.pipeline,
            &webrtc,
            id,
            &self.decoder,
            frame_tx,
            received.clone(),
            recv_bin.clone(),
            self.events.clone(),
        );

        let name = Arc::new(Mutex::new(name.to_string()));
        let spout_stop = Arc::new(AtomicBool::new(false));
        let spout_thread = spawn_spout_thread(
            id.to_string(),
            frame_rx,
            name.clone(),
            spout_stop.clone(),
            self.room_size,
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
                added_at: Instant::now(),
                last_video_at: None,
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

    fn announce_sending(&self, sender_name: &str, packer: &Packer) {
        (self.events)(Event::Info(format!(
            "Sending \"{sender_name}\" ({}×{}) at {}×{}.",
            packer.source_width, packer.source_height, packer.width, packer.height
        )));
    }

    /// Spread the upload budget over the partners (we send one stream to each).
    fn update_bitrate(&self) {
        let n = self.partners.len().max(1) as u32;
        let (min, max) = bitrate_range(self.transmit);
        let full = (UPLOAD_BUDGET_KBPS / n).clamp(min, max);
        let kbps = (full as f64 * self.quality).round() as u32;
        if let Some(enc) = &self.encoder {
            if enc.find_property("bitrate").is_some() {
                enc.set_property_from_str("bitrate", &kbps.to_string());
            }
        }
        if !self.partners.is_empty() && !self.closing {
            let total = kbps as f64 * self.partners.len() as f64 / 1000.0;
            let lowered = if self.quality < 1.0 { " (lowered: partners are losing packets)" } else { "" };
            self.events.clone()(Event::Info(format!(
                "Quality: {:.1} Mbit/s per partner, {total:.1} Mbit/s upload in total{lowered}.",
                kbps as f64 / 1000.0
            )));
        }
    }

    /// After each 5 s report: adjust quality to the worst loss partners report about
    /// our video (see LOSS_HIGH_PERCENT).
    fn adapt_quality(&mut self, worst_loss: Option<f64>) {
        let Some(loss) = worst_loss else { return };
        let (bad, good) = &mut self.loss_reports;
        let before = self.quality;
        if loss >= LOSS_HIGH_PERCENT {
            (*bad, *good) = (*bad + 1, 0);
            if *bad >= 2 {
                *bad = 0;
                self.quality = (self.quality * 0.8).max(QUALITY_MIN);
            }
        } else if loss < LOSS_LOW_PERCENT {
            (*bad, *good) = (0, *good + 1);
            if *good >= 6 {
                *good = 0;
                self.quality = (self.quality * 1.15).min(1.0);
            }
        } else {
            (*bad, *good) = (0, 0);
        }
        if self.quality != before {
            crate::log::info(&format!(
                "Partners lose {loss:.1}% of our video: quality {:.0}% -> {:.0}%",
                before * 100.0,
                self.quality * 100.0
            ));
            self.update_bitrate();
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
        self.announce_sending(sender_name, &packer);

        let frame_ns = 1_000_000_000 / FPS;
        let frame_interval = Duration::from_nanos(frame_ns);
        let start = Instant::now();
        let (mut sent, mut last_sent) = (0u64, 0u64);
        let mut last_report = Instant::now();

        let mut i: u64 = 0;
        while !stop.load(Ordering::Relaxed) {
            tick(self)?;
            self.sync_group_source();

            // Until the first frame goes out, the send size follows the avatar's size.
            if !self.sent_any && (packer.source_width, packer.source_height) != self.source_size {
                self.source_size = (packer.source_width, packer.source_height);
                self.refit()?;
            }

            // Reopen the avatar if the VTubing app restarted or changed its size (it's
            // scaled to the transmit size, so any size is fine), or if the transmit
            // size changed (joining: the room's settings arrived).
            let size_changed = (packer.width, packer.height) != self.transmit;
            if i % FPS == 0 || size_changed {
                if let Some(now) = spout::read_sender_info(sender_name) {
                    if now.share_handle != packer.share_handle || size_changed {
                        packer = Packer::open(&now, self.transmit)?;
                        self.announce_sending(sender_name, &packer);
                    }
                }
            }

            // Only capture and encode when someone is there to receive it.
            if !self.partners.is_empty() {
                let layout = Nv12Layout {
                    y_offset: self.send_info.offset()[0],
                    y_stride: self.send_info.stride()[0] as usize,
                    uv_offset: self.send_info.offset()[1],
                    uv_stride: self.send_info.stride()[1] as usize,
                };
                let frame_size = self.send_info.size();
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
                self.sent_any = true;
                sent += 1;
            }

            let since = last_report.elapsed();
            if since >= Duration::from_secs(5) {
                let secs = since.as_secs_f64();
                let mut received = Vec::new();
                let mut worst_sent_loss: Option<f64> = None;
                for (id, p) in self.partners.iter_mut() {
                    let now = p.received.load(Ordering::Relaxed);
                    if now > p.received_at_last_report {
                        p.last_video_at = Some(Instant::now());
                    }
                    let quiet = match p.last_video_at {
                        Some(t) => t.elapsed() > PARTNER_TIMEOUT,
                        None => p.added_at.elapsed() > FIRST_VIDEO_TIMEOUT,
                    };
                    if quiet && !self.lost.contains(id) {
                        self.lost.push(id.clone());
                    }
                    let net = network_numbers(p);
                    if let Some(l) = net.sent_loss_percent {
                        worst_sent_loss = Some(worst_sent_loss.map_or(l, |w: f64| w.max(l)));
                    }
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
                self.adapt_quality(worst_sent_loss);
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
///   * "remote-inbound-rtp": what the partner reports about our video — round-trip time,
///     and the share of our packets they lost lately ("fraction-lost", 0–1)
fn network_numbers(p: &mut Partner) -> NetNumbers {
    use gstreamer_webrtc::WebRTCStatsType;
    let Some(stats) = p.net_stats.lock().unwrap().clone() else { return NetNumbers::default() };

    let (mut lost, mut received, mut requests, mut rtt) = (None::<i64>, None::<u64>, None::<u32>, None::<f64>);
    let mut sent_loss = None::<f64>;
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
                sent_loss = entry.get::<f64>("fraction-lost").ok().map(|f| f * 100.0).or(sent_loss);
            }
            _ => {}
        }
    }

    let mut out = NetNumbers {
        round_trip_ms: rtt.map(|seconds| seconds * 1000.0),
        sent_loss_percent: sent_loss,
        ..Default::default()
    };
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
    /// Share of OUR video the partner lost lately (from their reports).
    sent_loss_percent: Option<f64>,
}

/// Change a connection's receive buffer while it runs. webrtcbin passes its "latency"
/// on to connections made later; the jitter buffers already running inside it are
/// set directly too. Returns how many of those there were.
fn set_latency(webrtc: &gst::Element, ms: u32) -> usize {
    webrtc.set_property("latency", ms);
    let Some(bin) = webrtc.downcast_ref::<gst::Bin>() else { return 0 };
    let mut changed = 0;
    for element in bin.iterate_recurse().into_iter().flatten() {
        if element.factory().is_some_and(|f| f.name() == "rtpjitterbuffer") {
            element.set_property("latency", ms);
            changed += 1;
        }
    }
    changed
}

/// When a partner's video stream appears, attach: depacketiser → decoder → appsink.
#[allow(clippy::too_many_arguments)]
fn attach_receiver(
    pipeline: &gst::Pipeline,
    webrtc: &gst::Element,
    peer_id: &str,
    decoder: &str,
    frame_tx: mpsc::SyncSender<Frame>,
    received: Arc<AtomicU64>,
    recv_bin: Arc<Mutex<Option<gst::Bin>>>,
    events: EventSink,
) {
    let pipeline_weak = pipeline.downgrade(); // "weak" = doesn't keep the pipeline alive
    let peer_id = peer_id.to_string();
    let decoder = decoder.to_string();
    webrtc.connect_pad_added(move |_, pad| {
        let Some(pipeline) = pipeline_weak.upgrade() else { return };
        if pad.direction() != gst::PadDirection::Src {
            return;
        }
        // If video packets go missing (and resending them didn't arrive in time), don't
        // show damaged frames — they smear until the next full frame. Instead skip ahead
        // to the next full frame ("keyframe") and ask the partner to send one right away.
        // The decoder (see `pick_decoder`) hands us NV12 in normal memory; the GPU
        // unpack shader does the rest.
        let bin = match gst::parse::bin_from_description(
            &format!(
                "rtph264depay request-keyframe=true wait-for-keyframe=true \
                 ! h264parse ! {decoder} ! video/x-raw,format=NV12 \
                 ! appsink name=sink sync=false max-buffers=2 drop=true"
            ),
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
    out_size: (u32, u32),
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
                // Always the room's size, even if the partner sent it smaller.
                let sender = SpoutSender::new(&source, out_size.0, out_size.1).map_err(|e| e.to_string())?;
                let unpacker =
                    Unpacker::new(sender.device(), f.w as u32, f.h as u32, out_size).map_err(|e| e.to_string())?;
                for layer in layers.lock().unwrap().iter_mut().filter(|l| l.id == peer_id) {
                    layer.source = source.clone();
                }
                events(Event::Info(format!(
                    "{source}: {}×{} (received at {}×{})",
                    out_size.0, out_size.1, f.w, f.h
                )));
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

/// The H.264 decoder on the graphics card our avatar is on, and its description for
/// the log. GStreamer has one decoder per graphics card: "d3d11h264dec" for Windows'
/// default card, "d3d11h264device1dec" for the next, and so on. On laptops with two
/// cards the default is often the weak built-in one, which can't keep up with
/// several partners at once.
///
/// TEST ONLY: VTC_DECODER=d3d11h264device1dec forces a decoder (e.g. to test an
/// AMD/Intel decoder on a PC whose default card is NVIDIA).
fn pick_decoder() -> (String, String) {
    let label = |name: &str| {
        gst::ElementFactory::find(name)
            .and_then(|f| f.metadata("long-name").map(String::from))
            .unwrap_or_else(|| name.to_string())
    };
    if let Ok(name) = std::env::var("VTC_DECODER") {
        let l = label(&name);
        return (name, l);
    }
    if let Some(card) = vtc_core::grab::card_id_in_use() {
        let names = std::iter::once("d3d11h264dec".to_string()).chain((1..8).map(|i| format!("d3d11h264device{i}dec")));
        for name in names {
            let Ok(e) = gst::ElementFactory::make(&name).build() else { continue };
            if e.find_property("adapter-luid").is_some() && e.property::<i64>("adapter-luid") == card {
                let l = label(&name);
                return (name, l);
            }
        }
    }
    let name = "d3d11h264dec".to_string();
    let l = label(&name);
    (name, l)
}
