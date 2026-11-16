//! `watch`: capture a Spout2 sender continuously at 60 fps and report how long each
//! capture takes. This tells us whether reading frames off the GPU is fast enough
//! to keep up before we build encoding and sending on top of it.

use std::time::{Duration, Instant};

use crate::grab::Grabber;
use crate::spout;

/// Target rate: 60 frames per second = one frame every ~16.7 ms.
const TARGET_FPS: u32 = 60;

pub fn watch(name: &str, seconds: u32) {
    let Some(info) = spout::read_sender_info(name) else {
        eprintln!("No Spout2 sender called \"{name}\". Run `cargo run` to see the list.");
        return;
    };

    let mut grabber = match Grabber::open(&info) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("Couldn't open sender: {e}");
            return;
        }
    };

    if cfg!(debug_assertions) {
        // `cfg!(debug_assertions)` is true for normal (debug) builds, which are much slower.
        println!("Note: this is a debug build. Use `cargo run --release -- watch ...` for real timing.\n");
    }

    println!(
        "Watching \"{name}\" ({}×{}) at {TARGET_FPS} fps for {seconds} s...",
        grabber.width, grabber.height
    );

    let frame_interval = Duration::from_secs(1) / TARGET_FPS;
    let total_frames = seconds * TARGET_FPS;

    let mut buffer = Vec::new();
    let mut grab_times: Vec<Duration> = Vec::with_capacity(total_frames as usize);
    let mut late_frames = 0; // frames where the grab didn't finish before the next was due
    let mut changed_frames = 0; // frames whose pixels differed from the previous one
    let mut reopens = 0;
    let mut last_fingerprint = 0u64;

    let start = Instant::now();
    for i in 0..total_frames {
        // Every second, check whether the sender restarted or resized (new handle).
        if i % TARGET_FPS == 0 {
            match spout::read_sender_info(name) {
                Some(now) if now.share_handle != grabber.share_handle => {
                    match Grabber::open(&now) {
                        Ok(g) => {
                            grabber = g;
                            reopens += 1;
                        }
                        Err(e) => {
                            eprintln!("Sender changed and couldn't be reopened: {e}");
                            return;
                        }
                    }
                }
                Some(_) => {}
                None => {
                    eprintln!("Sender \"{name}\" disappeared.");
                    break;
                }
            }
        }

        // Time just the capture itself.
        let t0 = Instant::now();
        if let Err(e) = grabber.grab(&mut buffer) {
            eprintln!("Grab failed on frame {i}: {e}");
            return;
        }
        let took = t0.elapsed();
        grab_times.push(took);
        if took > frame_interval {
            late_frames += 1;
        }

        // Did the image change? Compare a cheap fingerprint of every 64th byte.
        let fp = fingerprint(&buffer);
        if fp != last_fingerprint {
            changed_frames += 1;
            last_fingerprint = fp;
        }

        // Sleep until the next frame is due (keeps a steady 60 fps pace).
        let next_due = start + frame_interval * (i + 1);
        let now = Instant::now();
        if next_due > now {
            std::thread::sleep(next_due - now);
        }
    }
    let elapsed = start.elapsed().as_secs_f64();

    report(&mut grab_times, elapsed, late_frames, changed_frames, reopens);
}

/// Cheap "did anything change" check: mixes every 64th byte into one number.
fn fingerprint(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .step_by(64)
        .fold(0xcbf29ce484222325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

fn report(times: &mut [Duration], elapsed: f64, late: u32, changed: u32, reopens: u32) {
    if times.is_empty() {
        println!("No frames captured.");
        return;
    }
    times.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let n = times.len();
    let avg = times.iter().map(|&d| ms(d)).sum::<f64>() / n as f64;
    let pct = |p: f64| ms(times[((n as f64 - 1.0) * p) as usize]);

    println!("\nCaptured {n} frames in {elapsed:.1} s ({:.1} fps)", n as f64 / elapsed);
    println!("Grab time per frame (GPU copy + readback):");
    println!("  min {:.2} ms | avg {avg:.2} ms | 99% under {:.2} ms | max {:.2} ms",
        ms(times[0]), pct(0.99), ms(times[n - 1]));
    println!("  budget at {TARGET_FPS} fps: {:.2} ms", 1000.0 / TARGET_FPS as f64);
    println!("Late frames (grab took longer than budget): {late}");
    println!("Frames where the image changed: {changed} ({:.1} fps from the sender)",
        changed as f64 / elapsed);
    if reopens > 0 {
        println!("Sender restarted/resized {reopens} time(s); reopened automatically.");
    }
}
