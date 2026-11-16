//! encode-test: proves the "transparent video over H.264" trick on one PC, no network.
//!
//!   capture (Spout2) → pack colour + alpha side by side → NVIDIA H.264 encode
//!   → decode → unpack → publish as Spout2 "VTube Collab test" (for OBS)
//!   + compare one frame with the original
//!
//! Usage (release build for real timing):
//!   cargo run --release -p encode-test -- "Avatar Live" [seconds=10] [bitrate_kbps=8000]
//!
//! Why side by side? H.264 has no transparency channel. So the left half of each frame
//! carries the colours, and the right half carries the alpha as a grey image
//! (white = solid, black = see-through). The receiver splits them back apart.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// `as gst` gives the library a short nickname.
use gstreamer as gst;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
// The "prelude" brings in the extra methods GStreamer objects have (by_name, set_state...).
use gst::prelude::*;

use spout_probe::{grab::Grabber, sender::SpoutSender, spout};

const FPS: u64 = 60;

/// Name of the Spout2 source this test publishes (pick it in OBS).
const SPOUT_OUT_NAME: &str = "VTube Collab test";

/// Which frame (by number) we keep for the before/after quality comparison.
const COMPARE_FRAME: u64 = 120;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let name = args.first().map(String::as_str).unwrap_or("Avatar Live");
    let seconds: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(10);
    let bitrate_kbps: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(8000);

    if let Err(e) = run(name, seconds, bitrate_kbps) {
        eprintln!("Error: {e}");
    }
}

/// Data the decoder side hands back to the main thread.
/// `Arc<Mutex<...>>` = shared between threads (Arc), one thread at a time (Mutex).
#[derive(Default)]
struct Results {
    encode_latency: Vec<Duration>, // capture pushed → encoded frame ready
    total_latency: Vec<Duration>,  // capture pushed → decoded frame ready
    decoded_compare: Option<(Vec<u8>, usize)>, // (pixels, row stride) of COMPARE_FRAME
}

/// `Box<dyn std::error::Error>` = "any kind of error". Lets us use `?` on errors
/// from both GStreamer and Windows in the same function.
fn run(name: &str, seconds: u64, bitrate_kbps: u32) -> Result<(), Box<dyn std::error::Error>> {
    // ---- 1. Open the Spout2 sender (code shared from spout-probe) ----
    let info = spout::read_sender_info(name)
        .ok_or_else(|| format!("No Spout2 sender called \"{name}\""))?;
    let mut grabber = Grabber::open(&info)?;
    let (w, h) = (grabber.width as usize, grabber.height as usize);

    // ---- 2. Build the GStreamer pipeline ----
    // Written as text, like a chain: each `!` passes frames to the next element.
    gst::init()?;
    let pipeline_text = format!(
        "appsrc name=src is-live=true format=time \
             caps=video/x-raw,format=RGBA,width={pw},height={h},framerate={FPS}/1 \
         ! nvh264enc preset=p1 tune=ultra-low-latency zerolatency=true \
             rc-mode=cbr bitrate={bitrate_kbps} gop-size={FPS} bframes=0 \
         ! h264parse name=parse \
         ! nvh264dec \
         ! videoconvert n-threads=0 ! video/x-raw,format=RGBA \
         ! appsink name=sink sync=false max-buffers=4",
        pw = w * 2,
    );
    let pipeline = gst::parse::launch(&pipeline_text)?
        .downcast::<gst::Pipeline>()
        .map_err(|_| "not a pipeline")?;

    let appsrc = pipeline
        .by_name("src")
        .ok_or("no src")?
        .downcast::<gst_app::AppSrc>()
        .map_err(|_| "src is not an appsrc")?;
    let appsink = pipeline
        .by_name("sink")
        .ok_or("no sink")?
        .downcast::<gst_app::AppSink>()
        .map_err(|_| "sink is not an appsink")?;

    // Shared state between our loop and GStreamer's threads.
    let push_times: Arc<Mutex<HashMap<u64, Instant>>> = Arc::default();
    let results: Arc<Mutex<Results>> = Arc::default();
    let encoded_bytes = Arc::new(AtomicU64::new(0));
    let frame_ns = 1_000_000_000 / FPS;
    // One frame counter per measuring point (see `FrameCounter` below).
    let encoded_frames = FrameCounter::new(frame_ns);
    let sink_frames = FrameCounter::new(frame_ns);

    // ---- 3. Watch encoded frames as they leave h264parse (size + encode latency) ----
    {
        // `clone()` on an Arc makes another handle to the same shared data.
        let push_times = push_times.clone();
        let results = results.clone();
        let encoded_bytes = encoded_bytes.clone();
        let pad = pipeline.by_name("parse").ok_or("no parse")?.static_pad("src").ok_or("no pad")?;
        // `move` = the closure takes ownership of the handles above.
        pad.add_probe(gst::PadProbeType::BUFFER, move |_, probe| {
            if let Some(buf) = probe.buffer() {
                encoded_bytes.fetch_add(buf.size() as u64, Ordering::Relaxed);
                if let Some(pts) = buf.pts() {
                    let frame = encoded_frames.index(pts.nseconds());
                    // Skip the first second: the encoder is still warming up.
                    if let Some(t0) = push_times.lock().unwrap().get(&frame).filter(|_| frame >= FPS) {
                        results.lock().unwrap().encode_latency.push(t0.elapsed());
                    }
                }
            }
            gst::PadProbeReturn::Ok
        });
    }

    // ---- 4a. Spout output thread: unpacks decoded frames and publishes them ----
    // A "channel" passes data between threads. `sync_channel(2)` holds at most 2 frames;
    // if the Spout thread falls behind, extra frames are dropped instead of piling up.
    let (frame_tx, frame_rx) = std::sync::mpsc::sync_channel::<(Vec<u8>, usize)>(2);
    let out_name = SPOUT_OUT_NAME.to_string();
    let spout_thread = std::thread::spawn(move || -> Result<u64, String> {
        // The sender is created inside this thread and lives only here.
        let mut sender = SpoutSender::new(&out_name, w as u32, h as u32).map_err(|e| e.to_string())?;
        let mut sent = 0;
        // `for ... in frame_rx` waits for frames until the channel closes.
        for (packed, stride) in frame_rx {
            let rgba = unpack(&packed, stride, w, h);
            sender.send(&rgba).map_err(|e| e.to_string())?;
            sent += 1;
        }
        Ok(sent)
    });

    // ---- 4b. Receive decoded frames at the end of the pipeline ----
    {
        let push_times = push_times.clone();
        let results = results.clone();
        appsink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                    let Some(frame) = buffer.pts().map(|t| sink_frames.index(t.nseconds())) else {
                        return Ok(gst::FlowSuccess::Ok);
                    };

                    let mut r = results.lock().unwrap();
                    if let Some(t0) = push_times.lock().unwrap().remove(&frame) {
                        if frame >= FPS {
                            // Skip the first second (warm-up), as above.
                            r.total_latency.push(t0.elapsed());
                        }
                    }
                    // Copy the decoded frame out, with its row stride.
                    let caps = sample.caps().ok_or(gst::FlowError::Error)?;
                    let vinfo =
                        gst_video::VideoInfo::from_caps(caps).map_err(|_| gst::FlowError::Error)?;
                    let stride = vinfo.stride()[0] as usize;
                    let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                    let pixels = map.as_slice().to_vec();

                    if frame == COMPARE_FRAME {
                        r.decoded_compare = Some((pixels.clone(), stride));
                    }
                    // Hand it to the Spout thread; if it's busy, skip this frame.
                    let _ = frame_tx.try_send((pixels, stride));
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
    }

    pipeline.set_state(gst::State::Playing)?;
    println!(
        "Encoding \"{name}\" {w}×{h} → packed {}×{h} @ {FPS} fps, {bitrate_kbps} kbps, {seconds} s...",
        w * 2
    );

    // ---- 5. Capture, pack and push frames at a steady 60 fps ----
    let mut rgba = Vec::new();
    let mut original_compare: Option<Vec<u8>> = None;
    let mut pack_times = Vec::new();
    let frame_interval = Duration::from_nanos(frame_ns);
    let start = Instant::now();

    for i in 0..seconds * FPS {
        grabber.grab(&mut rgba)?;
        if i == COMPARE_FRAME {
            original_compare = Some(rgba.clone());
        }

        let t_pack = Instant::now();
        let packed = pack(&rgba, w, h);
        pack_times.push(t_pack.elapsed());

        // Wrap our bytes in a GStreamer buffer and stamp it with its time position.
        let pts = i * frame_ns;
        let mut buffer = gst::Buffer::from_mut_slice(packed);
        {
            let b = buffer.get_mut().ok_or("buffer not writable")?;
            b.set_pts(gst::ClockTime::from_nseconds(pts));
            b.set_duration(gst::ClockTime::from_nseconds(frame_ns));
        }
        push_times.lock().unwrap().insert(i, Instant::now());
        appsrc.push_buffer(buffer)?;

        let next_due = start + frame_interval * (i as u32 + 1);
        let now = Instant::now();
        if next_due > now {
            std::thread::sleep(next_due - now);
        }
    }
    let elapsed = start.elapsed().as_secs_f64();

    // ---- 6. Finish: tell the pipeline no more frames are coming, wait for it to drain ----
    appsrc.end_of_stream()?;
    let bus = pipeline.bus().ok_or("no bus")?;
    if let Some(msg) = bus.timed_pop_filtered(
        gst::ClockTime::from_seconds(5),
        &[gst::MessageType::Eos, gst::MessageType::Error],
    ) {
        if let gst::MessageView::Error(err) = msg.view() {
            pipeline.set_state(gst::State::Null)?;
            return Err(format!("GStreamer error: {} ({:?})", err.error(), err.debug()).into());
        }
    }
    pipeline.set_state(gst::State::Null)?;

    // Dropping the pipeline drops the appsink callback and its channel end, which
    // tells the Spout thread "no more frames". Then wait for it to finish.
    drop(appsink);
    drop(pipeline);
    match spout_thread.join() {
        Ok(Ok(sent)) => println!("\nSpout output \"{SPOUT_OUT_NAME}\": {sent} frames published"),
        Ok(Err(e)) => println!("\nSpout output failed: {e}"),
        Err(_) => println!("\nSpout output thread crashed"),
    }

    // ---- 7. Report ----
    let mut r = results.lock().unwrap();
    let mbps = encoded_bytes.load(Ordering::Relaxed) as f64 * 8.0 / elapsed / 1_000_000.0;
    println!("\nActual bitrate: {mbps:.2} Mbit/s");
    print_stats("Pack (CPU)", &mut pack_times);
    print_stats("Capture→encoded", &mut r.encode_latency);
    print_stats("Capture→decoded (total)", &mut r.total_latency);

    match (original_compare, r.decoded_compare.take()) {
        (Some(orig), Some((decoded, stride))) => {
            let roundtrip = unpack(&decoded, stride, w, h);
            compare(&orig, &roundtrip);
            std::fs::create_dir_all("captures")?;
            image::save_buffer("captures/original.png", &orig, w as u32, h as u32, image::ExtendedColorType::Rgba8)?;
            image::save_buffer("captures/roundtrip.png", &roundtrip, w as u32, h as u32, image::ExtendedColorType::Rgba8)?;
            println!("Saved captures/original.png and captures/roundtrip.png");
        }
        _ => println!("Comparison frame wasn't captured (run for at least 3 seconds)."),
    }
    Ok(())
}

/// Turns a timestamp back into a frame number (0, 1, 2...).
///
/// The encoder shifts every timestamp by a fixed amount (1000 hours, so they can
/// never go negative). So we remember the first timestamp seen at this point in the
/// pipeline and count frames from there. Frame 0 always arrives first.
struct FrameCounter {
    first_pts: AtomicU64, // u64::MAX = "not seen yet"
    frame_ns: u64,
}

impl FrameCounter {
    fn new(frame_ns: u64) -> FrameCounter {
        FrameCounter { first_pts: AtomicU64::new(u64::MAX), frame_ns }
    }

    fn index(&self, pts: u64) -> u64 {
        // Store `pts` only if nothing was stored yet; either way, get the first one back.
        let first = match self.first_pts.compare_exchange(u64::MAX, pts, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => pts,
            Err(existing) => existing,
        };
        // Round to the nearest frame (timestamps can be off by a nanosecond or two).
        (pts.saturating_sub(first) + self.frame_ns / 2) / self.frame_ns
    }
}

/// RGBA w×h → RGBA (2w)×h. Left: colour (made opaque). Right: alpha as grey.
/// Fully transparent pixels get black colour: their colour is invisible anyway,
/// and black compresses better than leftover junk.
fn pack(rgba: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * 2 * h * 4];
    for y in 0..h {
        let src = &rgba[y * w * 4..(y + 1) * w * 4];
        let dst = &mut out[y * w * 8..(y + 1) * w * 8];
        // Split the output row into its left (colour) and right (alpha) halves.
        let (left, right) = dst.split_at_mut(w * 4);
        for x in 0..w {
            let [r, g, b, a] = [src[x * 4], src[x * 4 + 1], src[x * 4 + 2], src[x * 4 + 3]];
            let colour = if a == 0 { [0, 0, 0, 255] } else { [r, g, b, 255] };
            left[x * 4..x * 4 + 4].copy_from_slice(&colour);
            right[x * 4..x * 4 + 4].copy_from_slice(&[a, a, a, 255]);
        }
    }
    out
}

/// The reverse of `pack`: colour from the left half, alpha from the right half's green.
/// `stride` = bytes per row in the decoded frame (may include padding).
fn unpack(packed: &[u8], stride: usize, w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * h * 4];
    for y in 0..h {
        let row = &packed[y * stride..];
        for x in 0..w {
            let c = &row[x * 4..x * 4 + 3];
            let a = row[(w + x) * 4 + 1];
            out[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[c[0], c[1], c[2], a]);
        }
    }
    out
}

/// Compare original vs round-tripped frame: colour quality and alpha accuracy.
fn compare(orig: &[u8], out: &[u8]) {
    let mut colour_sq_err = 0f64;
    let mut colour_n = 0u64;
    let mut alpha_abs_err = 0u64;
    let mut alpha_max_err = 0u8;
    let mut haze = 0u64; // was fully transparent, came back slightly visible
    let mut haze_max = 0u8;

    for (o, d) in orig.chunks_exact(4).zip(out.chunks_exact(4)) {
        let err = o[3].abs_diff(d[3]);
        alpha_abs_err += err as u64;
        alpha_max_err = alpha_max_err.max(err);
        if o[3] == 0 && d[3] > 0 {
            haze += 1;
            haze_max = haze_max.max(d[3]);
        }
        // Only judge colour where the pixel is mostly visible.
        if o[3] >= 128 {
            for c in 0..3 {
                let e = o[c] as f64 - d[c] as f64;
                colour_sq_err += e * e;
            }
            colour_n += 3;
        }
    }

    let pixels = (orig.len() / 4) as f64;
    println!("\nQuality (frame {COMPARE_FRAME}):");
    if colour_n > 0 {
        let mse = colour_sq_err / colour_n as f64;
        let psnr = if mse == 0.0 { f64::INFINITY } else { 10.0 * (255.0 * 255.0 / mse).log10() };
        println!("  colour PSNR on visible pixels: {psnr:.1} dB  (>40 excellent, 35–40 good, <30 visibly worse)");
    }
    println!("  alpha average error: {:.2} (out of 255), worst: {alpha_max_err}", alpha_abs_err as f64 / pixels);
    println!(
        "  new haze (was transparent, now not): {:.2}% of pixels, strongest alpha {haze_max}",
        haze as f64 * 100.0 / pixels
    );
}

fn print_stats(label: &str, times: &mut [Duration]) {
    if times.is_empty() {
        println!("{label}: no data");
        return;
    }
    times.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let n = times.len();
    let avg = times.iter().map(|&d| ms(d)).sum::<f64>() / n as f64;
    println!(
        "{label:<24} avg {avg:6.2} ms | 99% under {:6.2} ms | max {:6.2} ms  ({n} frames)",
        ms(times[(n - 1) * 99 / 100]),
        ms(times[n - 1])
    );
}
