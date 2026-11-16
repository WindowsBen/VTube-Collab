//! VTube Collab (command-line version).
//!
//!   collab host "<Spout sender>" [--name <your name>]
//!       start a collab: prints an invite code, then asks for the reply code
//!   collab join "<Spout sender>" [--name <your name>]
//!       join a collab: asks for the invite code, prints a reply code
//!
//! Both sides send their avatar and receive the partner's, which appears in OBS as
//! Spout2 source "Collab - <partner's name>". Your name defaults to the sender name.

mod code;
mod console;
mod encoder;
mod host;
mod join;
mod peer;
mod session;

const USAGE: &str = "Usage:\n  collab host \"<Spout sender>\" [--name <your name>]\n  collab join \"<Spout sender>\" [--name <your name>]";

fn main() {
    // GStreamer prints some harmless internal "ERROR" lines (e.g. about empty RTCP
    // reports). Hide its log unless GST_DEBUG is set for debugging.
    // Real problems still reach us through the pipeline's bus (see peer::watch_bus).
    if std::env::var_os("GST_DEBUG").is_none() {
        gstreamer::log::set_default_threshold(gstreamer::DebugLevel::None);
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str);
    let sender = args.get(1).filter(|s| !s.starts_with("--"));
    // `--name X` anywhere after the mode; default: the sender name.
    let name = args
        .iter()
        .position(|a| a == "--name")
        .and_then(|i| args.get(i + 1))
        .or(sender);

    let result = match (mode, sender, name) {
        (Some("host"), Some(sender), Some(name)) => host::run(sender, name),
        (Some("join"), Some(sender), Some(name)) => join::run(sender, name),
        _ => Err(USAGE.into()),
    };
    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
