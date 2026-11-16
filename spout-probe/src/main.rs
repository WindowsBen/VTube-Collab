//! spout-probe: inspect Spout2 senders on this PC.
//!
//! Usage:
//!   cargo run                         list all senders
//!   cargo run -- grab "Avatar Live"   save one frame as captures\Avatar Live.png

// `mod` pulls in the code from src\spout.rs and src\grab.rs.
mod grab;
mod spout;

use std::path::PathBuf;

fn main() {
    // Command-line words after the program name, e.g. ["grab", "Avatar Live"].
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        None => list_senders(),
        Some("grab") => match args.get(1) {
            Some(name) => grab_sender(name),
            None => eprintln!("Usage: cargo run -- grab \"<sender name>\""),
        },
        Some(other) => eprintln!("Unknown command: {other}"),
    }
}

fn list_senders() {
    let names = spout::read_sender_names();

    if names.is_empty() {
        println!("No Spout2 senders found.");
        println!("Tip: open your VTubing app and turn on its Spout2 output, then run this again.");
        return;
    }

    println!("Found {} Spout2 sender(s):\n", names.len());
    for name in &names {
        match spout::read_sender_info(name) {
            Some(info) => {
                println!("  • {name}");
                println!("      size:   {} × {}", info.width, info.height);
                println!("      format: {} ({})", spout::format_name(info.format), info.format);
                let app = spout::c_bytes_to_string(&info.description);
                if !app.is_empty() {
                    println!("      app:    {app}");
                }
            }
            None => println!("  • {name}  (listed, but its details couldn't be read — it may have just closed)"),
        }
    }
}

fn grab_sender(name: &str) {
    let Some(info) = spout::read_sender_info(name) else {
        eprintln!("No Spout2 sender called \"{name}\". Run `cargo run` to see the list.");
        return;
    };

    let frame = match grab::grab_frame(&info) {
        Ok(frame) => frame,
        Err(e) => {
            eprintln!("Couldn't grab a frame: {e}");
            return;
        }
    };

    // Count fully transparent pixels: the 4th byte of each pixel is alpha (0 = see-through).
    let total = (frame.width * frame.height) as usize;
    let transparent = frame.rgba.chunks_exact(4).filter(|px| px[3] == 0).count();

    let path = PathBuf::from("captures").join(format!("{name}.png"));
    if let Err(e) = std::fs::create_dir_all("captures") {
        eprintln!("Couldn't create captures folder: {e}");
        return;
    }
    if let Err(e) = image::save_buffer(
        &path,
        &frame.rgba,
        frame.width,
        frame.height,
        image::ExtendedColorType::Rgba8,
    ) {
        eprintln!("Couldn't save PNG: {e}");
        return;
    }

    println!("Saved {}", path.display());
    println!("  size:        {} × {}", frame.width, frame.height);
    println!(
        "  transparent: {:.1}% of pixels",
        transparent as f64 * 100.0 / total as f64
    );

    // Breakdown of alpha values: shows soft edges / faint backgrounds, not just 0 or 255.
    println!("  alpha breakdown:");
    for (lo, hi) in [(0u8, 0u8), (1, 15), (16, 127), (128, 254), (255, 255)] {
        let n = frame.rgba.chunks_exact(4).filter(|px| (lo..=hi).contains(&px[3])).count();
        println!("    {lo:>3}-{hi:<3}: {:5.1}%", n as f64 * 100.0 / total as f64);
    }
    let corner = &frame.rgba[0..4];
    println!("  top-left pixel RGBA: {corner:?}");
}
