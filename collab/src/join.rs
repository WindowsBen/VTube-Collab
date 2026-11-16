//! `collab join`: receive a partner's avatar and publish it as a Spout2 source for OBS.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use gstreamer_webrtc as gst_webrtc;

use vtc_core::packing::unpack;
use vtc_core::sender::SpoutSender;

use crate::peer::{self, Error};
use crate::{code, console};

/// What we call ourselves in the reply code (shown on the host's screen).
const MY_NAME: &str = "guest";

pub fn run() -> Result<(), Error> {
    gst::init()?;

    // ---- Invite code first: it tells us who we're connecting to ----
    let invite = console::read_code("Paste the invite code: ")?;
    let spout_name = format!("Collab - {}", invite.name);

    // ---- Receiving pipeline: just webrtcbin for now; the decoder is added when video arrives ----
    let pipeline = gst::Pipeline::new();
    let webrtc = gst::ElementFactory::make("webrtcbin")
        .name("webrtc")
        .property_from_str("bundle-policy", "max-bundle")
        .property("stun-server", peer::STUN_SERVER)
        .property("latency", 50u32)
        .build()?;
    pipeline.add(&webrtc)?;

    // Decoded frames go from GStreamer's thread to our Spout thread through this channel.
    let (frame_tx, frame_rx) = std::sync::mpsc::sync_channel::<(Vec<u8>, usize, usize, usize)>(2);
    let received = Arc::new(AtomicU64::new(0));

    // Event: "a new incoming stream appeared" → attach depacketiser + decoder + appsink.
    {
        let pipeline_weak = pipeline.downgrade(); // "weak" = doesn't keep the pipeline alive
        let received = received.clone();
        webrtc.connect_pad_added(move |_, pad| {
            let Some(pipeline) = pipeline_weak.upgrade() else { return };
            if pad.direction() != gst::PadDirection::Src {
                return;
            }
            // d3d11h264dec decodes on any graphics card (NVIDIA, AMD or Intel).
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
                        // Packed frame is twice as wide as the avatar.
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
            if let Err(e) = pad.link(&sink_pad) {
                eprintln!("[error] couldn't connect incoming video: {e:?}");
            } else {
                println!("[video] incoming stream started");
            }
        });
    }

    // ---- Spout thread: unpack frames and publish them for OBS ----
    // The sender is created when the first frame arrives (that's when we know the size).
    let out_name = spout_name.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let spout_stop = stop.clone();
    let spout_thread = std::thread::spawn(move || -> Result<(), String> {
        let mut sender: Option<SpoutSender> = None;
        // Wait for frames, but check the stop flag at least every 100 ms so we always
        // exit (and remove our Spout source) when asked, even if frames keep coming.
        while !spout_stop.load(Ordering::Relaxed) {
            let (packed, stride, w, h) = match frame_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(frame) => frame,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            };
            if sender.is_none() {
                sender = Some(SpoutSender::new(&out_name, w as u32, h as u32).map_err(|e| e.to_string())?);
                println!("[spout] publishing \"{out_name}\" ({w}×{h}) — add it in OBS with Spout2 Capture");
            }
            let rgba = unpack(&packed, stride, w, h);
            sender.as_mut().unwrap().send(&rgba).map_err(|e| e.to_string())?;
        }
        Ok(())
    });

    peer::watch_bus(&pipeline);
    peer::log_connection_state(&webrtc);
    let gatherer = peer::Gatherer::attach(&webrtc);
    pipeline.set_state(gst::State::Playing)?;

    // ---- Answer with a reply code ----
    println!("Connecting to {}...", invite.name);
    peer::apply_remote(&webrtc, gst_webrtc::WebRTCSDPType::Offer, &invite)?;
    println!("Preparing reply code (finding your network addresses)...");
    let local_sdp = peer::create_local(&webrtc, gst_webrtc::WebRTCSDPType::Answer)?;
    let reply = code::encode(&gatherer.into_code(MY_NAME, local_sdp));
    console::show_code("REPLY", &reply);
    println!("Send this reply code back to {}. Press Enter here to stop.", invite.name);

    // ---- Report received frame rate until Enter is pressed ----
    let enter = console::stop_on_enter();
    let mut last = 0;
    let mut last_time = Instant::now();
    while !enter.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(200));
        if last_time.elapsed() >= Duration::from_secs(5) {
            let now = received.load(Ordering::Relaxed);
            if now > 0 {
                println!("[guest] {:.1} fps received", (now - last) as f64 / last_time.elapsed().as_secs_f64());
            }
            last = now;
            last_time = Instant::now();
        }
    }

    // Tell the Spout thread to finish (it removes its source on the way out), then shut down.
    stop.store(true, Ordering::Relaxed);
    match spout_thread.join() {
        Ok(Err(e)) => eprintln!("[spout] {e}"),
        Err(_) => eprintln!("[spout] thread crashed"),
        Ok(Ok(())) => {}
    }
    pipeline.set_state(gst::State::Null)?;
    println!("Stopped.");
    Ok(())
}
