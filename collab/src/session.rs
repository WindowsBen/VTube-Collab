//! One collab connection: sends our avatar AND receives the partner's.
//!
//!   send:    Spout texture → [GPU pack → NV12] → appsrc → H.264 encoder → RTP → webrtcbin ─┐
//!                                                                                           │ network
//!   receive: webrtcbin → RTP → H.264 decoder → NV12 → appsink → [GPU unpack] → Spout → OBS ┘
//!
//! The pixel work happens on the graphics card (vtc_core::gpu); the CPU only copies
//! compact NV12 frames. Host and guest use the same Session; they only differ in who
//! makes the offer.

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
use vtc_core::sender::SpoutSender;
use vtc_core::spout;

use crate::encoder;
use crate::events::{Event, EventSink};
use crate::peer::{self, Error};

pub const FPS: u64 = 60;
const BITRATE_KBPS: u32 = 8000;

/// A decoded packed frame on its way to the Spout thread: the two NV12 planes.
struct Frame {
    y: Vec<u8>,
    y_stride: usize,
    uv: Vec<u8>,
    uv_stride: usize,
    /// Avatar size (the packed frame is twice as wide).
    w: usize,
    h: usize,
}

pub struct Session {
    pipeline: gst::Pipeline,
    pub webrtc: gst::Element,
    appsrc: gst_app::AppSrc,
    /// Memory layout of the packed NV12 frames we send.
    send_info: gst_video::VideoInfo,
    gatherer: Option<peer::Gatherer>,
    received: Arc<AtomicU64>,
    partner: Arc<Mutex<String>>,
    spout_stop: Arc<AtomicBool>,
    spout_thread: Option<JoinHandle<Result<(), String>>>,
    events: EventSink,
}

impl Session {
    /// Build the pipeline for sending a `w`×`h` avatar, and start it.
    pub fn start(w: usize, h: usize, events: EventSink) -> Result<Session, Error> {
        let enc = encoder::pick(BITRATE_KBPS, FPS)?;
        events(Event::Info(format!("Encoder: {}", enc.label)));

        // We send packed NV12 frames (twice the avatar width) in BT.709 limited range,
        // exactly what the GPU pack shader produces.
        let send_info = gst_video::VideoInfo::builder(gst_video::VideoFormat::Nv12, (w * 2) as u32, h as u32)
            .fps(gst::Fraction::new(FPS as i32, 1))
            .colorimetry(&"bt709".parse()?)
            .build()?;
        let caps = send_info.to_caps()?;

        let text = format!(
            "appsrc name=src is-live=true format=time caps=\"{caps}\" \
             ! {enc} \
             ! h264parse config-interval=-1 \
             ! rtph264pay config-interval=-1 aggregate-mode=zero-latency pt=96 \
             ! application/x-rtp,media=video,encoding-name=H264,payload=96,clock-rate=90000 \
             ! webrtcbin name=webrtc bundle-policy=max-bundle stun-server={stun} latency=50",
            enc = enc.pipeline,
            stun = peer::STUN_SERVER,
        );
        let pipeline = gst::parse::launch(&text)?
            .downcast::<gst::Pipeline>()
            .map_err(|_| "not a pipeline")?;
        let webrtc = pipeline.by_name("webrtc").ok_or("no webrtcbin")?;
        let appsrc = pipeline
            .by_name("src")
            .ok_or("no src")?
            .downcast::<gst_app::AppSrc>()
            .map_err(|_| "src is not an appsrc")?;

        let (frame_tx, frame_rx) = mpsc::sync_channel::<Frame>(2);
        let received = Arc::new(AtomicU64::new(0));
        attach_receiver(&pipeline, &webrtc, frame_tx, received.clone(), events.clone());

        let partner = Arc::new(Mutex::new(String::from("partner")));
        let spout_stop = Arc::new(AtomicBool::new(false));
        let spout_thread = spawn_spout_thread(frame_rx, partner.clone(), spout_stop.clone(), events.clone());

        peer::watch_bus(&pipeline, events.clone());
        peer::report_connection_state(&webrtc, events.clone());
        // Attach before negotiation starts, so no address is missed.
        let gatherer = peer::Gatherer::attach(&webrtc);
        pipeline.set_state(gst::State::Playing)?;

        Ok(Session {
            pipeline,
            webrtc,
            appsrc,
            send_info,
            gatherer: Some(gatherer),
            received,
            partner,
            spout_stop,
            spout_thread: Some(spout_thread),
            events,
        })
    }

    /// The address collector; can be taken once, when building our code.
    pub fn take_gatherer(&mut self) -> peer::Gatherer {
        self.gatherer.take().expect("gatherer is only taken once")
    }

    /// Name the partner (used for the Spout source "Collab - <name>").
    pub fn set_partner(&self, name: &str) {
        *self.partner.lock().unwrap() = name.to_string();
    }

    /// Capture, send and report until `stop` becomes true, then shut down cleanly.
    pub fn run(mut self, sender_name: &str, packer: Packer, stop: &AtomicBool) -> Result<(), Error> {
        let result = self.send_loop(sender_name, packer, stop);
        self.shutdown(); // always clean up (removes our Spout source), even after an error
        result
    }

    fn send_loop(&mut self, sender_name: &str, mut packer: Packer, stop: &AtomicBool) -> Result<(), Error> {
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
        let (mut sent, mut last_sent, mut last_received) = (0u64, 0u64, 0u64);
        let mut last_report = Instant::now();

        let mut i: u64 = 0;
        while !stop.load(Ordering::Relaxed) {
            // If the avatar app restarted, reopen it (size must stay the same for now).
            if i % FPS == 0 {
                if let Some(now) = spout::read_sender_info(sender_name) {
                    if now.share_handle != packer.share_handle {
                        let p = Packer::open(&now)?;
                        if (p.width, p.height) != (w, h) {
                            return Err("the avatar's resolution changed; restart collab".into());
                        }
                        packer = p;
                    }
                }
            }

            // Ask GStreamer for a buffer and let the GPU pack straight into it.
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

            let since = last_report.elapsed();
            if since >= Duration::from_secs(5) {
                let received = self.received.load(Ordering::Relaxed);
                (self.events)(Event::Stats {
                    sent_fps: (sent - last_sent) as f64 / since.as_secs_f64(),
                    received_fps: (received - last_received) as f64 / since.as_secs_f64(),
                });
                (last_sent, last_received, last_report) = (sent, received, Instant::now());
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

    fn shutdown(&mut self) {
        // Stop the Spout thread first: it removes our partner's source from OBS's list.
        self.spout_stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.spout_thread.take() {
            match t.join() {
                Ok(Err(e)) => (self.events)(Event::Warning(format!("Spout output: {e}"))),
                Err(_) => (self.events)(Event::Warning("Spout output thread crashed".into())),
                Ok(Ok(())) => {}
            }
        }
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

impl Drop for Session {
    /// If a session is abandoned before `run` (e.g. "Cancel" while waiting for a code),
    /// still stop the pipeline and remove the Spout source. Running this twice is harmless.
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// When the partner's video stream appears, attach: depacketiser → decoder → appsink.
fn attach_receiver(
    pipeline: &gst::Pipeline,
    webrtc: &gst::Element,
    frame_tx: mpsc::SyncSender<Frame>,
    received: Arc<AtomicU64>,
    events: EventSink,
) {
    let pipeline_weak = pipeline.downgrade(); // "weak" = doesn't keep the pipeline alive
    webrtc.connect_pad_added(move |_, pad| {
        let Some(pipeline) = pipeline_weak.upgrade() else { return };
        if pad.direction() != gst::PadDirection::Src {
            return;
        }
        // d3d11h264dec decodes on any graphics card (NVIDIA, AMD or Intel) and hands
        // us NV12 in normal memory; the GPU unpack shader does the rest.
        let bin = match gst::parse::bin_from_description(
            "rtph264depay ! h264parse ! d3d11h264dec ! video/x-raw,format=NV12 \
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
            Ok(_) => events(Event::PartnerVideo),
            Err(e) => events(Event::Warning(format!("couldn't connect incoming video: {e:?}"))),
        }
    });
}

/// Unpacks received frames on the GPU, straight into Spout2 "Collab - <partner>".
/// The Spout source is created on the first frame (that's when we know the size).
fn spawn_spout_thread(
    frame_rx: mpsc::Receiver<Frame>,
    partner: Arc<Mutex<String>>,
    stop: Arc<AtomicBool>,
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
                let name = format!("Collab - {}", partner.lock().unwrap());
                let sender = SpoutSender::new(&name, f.w as u32, f.h as u32).map_err(|e| e.to_string())?;
                let unpacker = Unpacker::new(sender.device(), f.w as u32, f.h as u32).map_err(|e| e.to_string())?;
                events(Event::Publishing(name));
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
