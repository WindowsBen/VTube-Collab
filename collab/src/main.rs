//! VTube Collab (command-line version).
//!
//!   collab host "Avatar Live"   send your avatar; prints an invite code
//!   collab join                 paste an invite code; receive your partner's avatar
//!                               as Spout2 source "Collab - <name>" for OBS

mod code;
mod console;
mod host;
mod join;
mod peer;

fn main() {
    // GStreamer prints some harmless internal "ERROR" lines (e.g. about empty RTCP
    // reports on send-only links). Hide its log unless GST_DEBUG is set for debugging.
    // Real problems still reach us through the pipeline's bus (see peer::watch_bus).
    if std::env::var_os("GST_DEBUG").is_none() {
        gstreamer::log::set_default_threshold(gstreamer::DebugLevel::None);
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("host") => match args.get(1) {
            Some(name) => host::run(name),
            None => Err("Usage: collab host \"<Spout sender name>\"".into()),
        },
        Some("join") => join::run(),
        _ => Err("Usage:\n  collab host \"<Spout sender name>\"\n  collab join".into()),
    };
    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
