//! `collab host "<Spout sender>"`: capture your avatar and send it to a guest.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app as gst_app;
use gstreamer_webrtc as gst_webrtc;

use vtc_core::grab::Grabber;
use vtc_core::packing::pack;
use vtc_core::spout;

use crate::peer::{self, Error};
use crate::{code, console};

const FPS: u64 = 60;
const BITRATE_KBPS: u32 = 8000;

pub fn run(sender_name: &str) -> Result<(), Error> {
    // ---- Open the avatar's Spout2 sender ----
    let info = spout::read_sender_info(sender_name)
        .ok_or_else(|| format!("No Spout2 sender called \"{sender_name}\". Run spout-probe to see the list."))?;
    let mut grabber = Grabber::open(&info)?;
    let (w, h) = (grabber.width as usize, grabber.height as usize);

    // ---- Build the sending pipeline ----
    // Same encoder settings as encode-test, then: packetise for the network (rtph264pay)
    // and hand to webrtcbin, which encrypts and sends over the direct connection.
    gst::init()?;
    let text = format!(
        "appsrc name=src is-live=true format=time \
             caps=video/x-raw,format=RGBA,width={pw},height={h},framerate={FPS}/1 \
         ! nvh264enc preset=p1 tune=ultra-low-latency zerolatency=true \
             rc-mode=cbr bitrate={BITRATE_KBPS} gop-size={FPS} bframes=0 \
         ! h264parse config-interval=-1 \
         ! rtph264pay config-interval=-1 aggregate-mode=zero-latency pt=96 \
         ! application/x-rtp,media=video,encoding-name=H264,payload=96,clock-rate=90000 \
         ! webrtcbin name=webrtc bundle-policy=max-bundle stun-server={stun} latency=50",
        pw = w * 2,
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

    // We only send in this step, so mark the video channel "send only".
    let transceiver = webrtc
        .emit_by_name::<Option<gst_webrtc::WebRTCRTPTransceiver>>("get-transceiver", &[&0i32])
        .ok_or("no video transceiver")?;
    transceiver.set_property("direction", gst_webrtc::WebRTCRTPTransceiverDirection::Sendonly);

    peer::watch_bus(&pipeline);
    peer::log_connection_state(&webrtc);
    let gatherer = peer::Gatherer::attach(&webrtc);
    pipeline.set_state(gst::State::Playing)?;

    // ---- Invite code ----
    println!("Preparing invite code (finding your network addresses)...");
    let local_sdp = peer::create_local(&webrtc, gst_webrtc::WebRTCSDPType::Offer)?;
    let invite = code::encode(&gatherer.into_code(sender_name, local_sdp));
    console::show_code("INVITE", &invite);
    println!("Send this invite code to your partner, then paste their reply code here.");

    // ---- Reply code ----
    let reply = console::read_code("Reply code: ")?;
    println!("Connecting to {}...", reply.name);
    peer::apply_remote(&webrtc, gst_webrtc::WebRTCSDPType::Answer, &reply)?;

    // ---- Send frames until Enter is pressed ----
    println!("Sending \"{sender_name}\" ({w}×{h}). Press Enter to stop.");
    let stop = console::stop_on_enter();
    let frame_ns = 1_000_000_000 / FPS;
    let frame_interval = Duration::from_nanos(frame_ns);
    let start = Instant::now();
    let mut rgba = Vec::new();
    let mut last_report = Instant::now();
    let mut sent_since_report = 0u32;

    let mut i: u64 = 0;
    while !stop.load(Ordering::Relaxed) {
        // If the avatar app restarted or resized, reopen it (size must stay the same for now).
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
        let mut buffer = gst::Buffer::from_mut_slice(pack(&rgba, w, h));
        {
            let b = buffer.get_mut().ok_or("buffer not writable")?;
            b.set_pts(gst::ClockTime::from_nseconds(i * frame_ns));
            b.set_duration(gst::ClockTime::from_nseconds(frame_ns));
        }
        appsrc.push_buffer(buffer)?;
        sent_since_report += 1;

        if last_report.elapsed() >= Duration::from_secs(5) {
            println!("[host] {:.1} fps sent", sent_since_report as f64 / last_report.elapsed().as_secs_f64());
            last_report = Instant::now();
            sent_since_report = 0;
        }

        i += 1;
        let next_due = start + frame_interval * i as u32;
        let now = Instant::now();
        if next_due > now {
            std::thread::sleep(next_due - now);
        }
    }

    pipeline.set_state(gst::State::Null)?;
    println!("Stopped.");
    Ok(())
}
