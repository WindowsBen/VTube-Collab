//! One collab connection: sends our avatar AND receives the partner's.
//!
//!   send:    our frames → appsrc → H.264 encoder → RTP packets → webrtcbin ─┐
//!                                                                            │ network
//!   receive: webrtcbin → RTP → H.264 decoder → appsink → Spout thread → OBS ┘
//!
//! Host and guest use the same Session; they only differ in who makes the offer.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;

use vtc_core::grab::Grabber;
use vtc_core::packing::{pack_into, unpack};
use vtc_core::sender::SpoutSender;
use vtc_core::spout;

use crate::console;
use crate::encoder;
use crate::peer::{self, Error};

pub const FPS: u64 = 60;
const BITRATE_KBPS: u32 = 8000;

/// A decoded frame on its way to the Spout thread: (pixels, row stride, width, height).
type Frame = (Vec<u8>, usize, usize, usize);

pub struct Session {
    pipeline: gst::Pipeline,
    pub webrtc: gst::Element,
    appsrc: gst_app::AppSrc,
    gatherer: Option<peer::Gatherer>,
    received: Arc<AtomicU64>,
    partner: Arc<Mutex<String>>,
    spout_stop: Arc<AtomicBool>,
    spout_thread: Option<JoinHandle<Result<(), String>>>,
}

impl Session {
    /// Build the pipeline for sending a `w`×`h` avatar, and start it.
    pub fn start(w: usize, h: usize) -> Result<Session, Error> {
        let enc = encoder::pick(BITRATE_KBPS, FPS)?;
        println!("Encoder: {}", enc.label);

        let text = format!(
            "appsrc name=src is-live=true format=time \
                 caps=video/x-raw,format=RGBA,width={pw},height={h},framerate={FPS}/1 \
             ! {enc} \
             ! h264parse config-interval=-1 \
             ! rtph264pay config-interval=-1 aggregate-mode=zero-latency pt=96 \
             ! application/x-rtp,media=video,encoding-name=H264,payload=96,clock-rate=90000 \
             ! webrtcbin name=webrtc bundle-policy=max-bundle stun-server={stun} latency=50",
            pw = w * 2,
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
        attach_receiver(&pipeline, &webrtc, frame_tx, received.clone());

        let partner = Arc::new(Mutex::new(String::from("partner")));
        let spout_stop = Arc::new(AtomicBool::new(false));
        let spout_thread = spawn_spout_thread(frame_rx, partner.clone(), spout_stop.clone());

        peer::watch_bus(&pipeline);
        peer::log_connection_state(&webrtc);
        // Attach before negotiation starts, so no address is missed.
        let gatherer = peer::Gatherer::attach(&webrtc);
        pipeline.set_state(gst::State::Playing)?;

        Ok(Session {
            pipeline,
            webrtc,
            appsrc,
            gatherer: Some(gatherer),
            received,
            partner,
            spout_stop,
            spout_thread: Some(spout_thread),
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

    /// Capture, send and report until Enter is pressed, then shut down cleanly.
    pub fn run(mut self, sender_name: &str, mut grabber: Grabber) -> Result<(), Error> {
        let (w, h) = (grabber.width as usize, grabber.height as usize);
        println!("Sending \"{sender_name}\" ({w}×{h}). Press Enter to stop.");
        let enter = console::stop_on_enter();

        let frame_ns = 1_000_000_000 / FPS;
        let frame_interval = Duration::from_nanos(frame_ns);
        let start = Instant::now();
        let mut rgba = Vec::new();
        let (mut sent, mut last_sent, mut last_received) = (0u64, 0u64, 0u64);
        let mut last_report = Instant::now();

        let mut i: u64 = 0;
        while !enter.load(Ordering::Relaxed) {
            // If the avatar app restarted, reopen it (size must stay the same for now).
            if i % FPS == 0 {
                if let Some(now) = spout::read_sender_info(sender_name) {
                    if now.share_handle != grabber.share_handle {
                        let g = Grabber::open(&now)?;
                        if (g.width as usize, g.height as usize) != (w, h) {
                            return Err("the avatar's resolution changed; restart collab".into());
                        }
                        grabber = g;
                    }
                }
            }

            grabber.grab(&mut rgba)?;
            // Ask GStreamer for a buffer and pack straight into it (no extra copy).
            let mut buffer = gst::Buffer::with_size(w * 2 * h * 4)?;
            {
                let b = buffer.get_mut().ok_or("buffer not writable")?;
                let mut map = b.map_writable()?;
                pack_into(&rgba, w, h, &mut map);
            }
            {
                let b = buffer.get_mut().ok_or("buffer not writable")?;
                b.set_pts(gst::ClockTime::from_nseconds(i * frame_ns));
                b.set_duration(gst::ClockTime::from_nseconds(frame_ns));
            }
            self.appsrc.push_buffer(buffer)?;
            sent += 1;

            let since = last_report.elapsed();
            if since >= Duration::from_secs(5) {
                let received = self.received.load(Ordering::Relaxed);
                println!(
                    "[stats] sent {:.1} fps | received {:.1} fps",
                    (sent - last_sent) as f64 / since.as_secs_f64(),
                    (received - last_received) as f64 / since.as_secs_f64(),
                );
                (last_sent, last_received, last_report) = (sent, received, Instant::now());
            }

            i += 1;
            let next_due = start + frame_interval * i as u32;
            let now = Instant::now();
            if next_due > now {
                std::thread::sleep(next_due - now);
            }
        }

        self.shutdown();
        println!("Stopped.");
        Ok(())
    }

    fn shutdown(&mut self) {
        // Stop the Spout thread first: it removes our partner's source from OBS's list.
        self.spout_stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.spout_thread.take() {
            match t.join() {
                Ok(Err(e)) => eprintln!("[spout] {e}"),
                Err(_) => eprintln!("[spout] thread crashed"),
                Ok(Ok(())) => {}
            }
        }
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// When the partner's video stream appears, attach: depacketiser → decoder → appsink.
fn attach_receiver(
    pipeline: &gst::Pipeline,
    webrtc: &gst::Element,
    frame_tx: mpsc::SyncSender<Frame>,
    received: Arc<AtomicU64>,
) {
    let pipeline_weak = pipeline.downgrade(); // "weak" = doesn't keep the pipeline alive
    webrtc.connect_pad_added(move |_, pad| {
        let Some(pipeline) = pipeline_weak.upgrade() else { return };
        if pad.direction() != gst::PadDirection::Src {
            return;
        }
        // d3d11h264dec decodes on any graphics card (NVIDIA, AMD or Intel).
        // Colour conversion stays on the CPU (videoconvert): the GPU path (d3d11convert)
        // was tried and shifted alpha levels (see-through came back as faint haze).
        let bin = match gst::parse::bin_from_description(
            "rtph264depay ! h264parse ! d3d11h264dec \
             ! videoconvert n-threads=0 ! video/x-raw,format=RGBA \
             ! appsink name=sink sync=false max-buffers=2 drop=true",
            true,
        ) {
            Ok(b) => b,
            Err(e) => return eprintln!("[error] couldn't build decoder: {e}"),
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
                    let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                    // The packed frame is twice as wide as the avatar.
                    let (w, h) = (info.width() as usize / 2, info.height() as usize);
                    let _ = frame_tx.try_send((map.as_slice().to_vec(), info.stride()[0] as usize, w, h));
                    received.fetch_add(1, Ordering::Relaxed);
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        if pipeline.add(&bin).is_err() || bin.sync_state_with_parent().is_err() {
            return eprintln!("[error] couldn't start decoder");
        }
        let sink_pad = bin.static_pad("sink").expect("bin has a sink pad");
        match pad.link(&sink_pad) {
            Ok(_) => println!("[video] partner's stream started"),
            Err(e) => eprintln!("[error] couldn't connect incoming video: {e:?}"),
        }
    });
}

/// Unpacks received frames and publishes them as Spout2 "Collab - <partner>".
/// The Spout source is created on the first frame (that's when we know the size).
fn spawn_spout_thread(
    frame_rx: mpsc::Receiver<Frame>,
    partner: Arc<Mutex<String>>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<Result<(), String>> {
    std::thread::spawn(move || {
        let mut sender: Option<SpoutSender> = None;
        // Check the stop flag at least every 100 ms, so we always exit when asked.
        while !stop.load(Ordering::Relaxed) {
            let (packed, stride, w, h) = match frame_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(frame) => frame,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            if sender.is_none() {
                let name = format!("Collab - {}", partner.lock().unwrap());
                sender = Some(SpoutSender::new(&name, w as u32, h as u32).map_err(|e| e.to_string())?);
                println!("[spout] publishing \"{name}\" ({w}×{h}) — add it in OBS with Spout2 Capture");
            }
            let rgba = unpack(&packed, stride, w, h);
            sender.as_mut().unwrap().send(&rgba).map_err(|e| e.to_string())?;
        }
        Ok(())
    })
}
