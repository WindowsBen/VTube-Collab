//! VTube Collab command-line tool (the app window is in `app/`).
//!
//!   collab list
//!       show the Spout2 sources on this PC (your avatar should be one of them)
//!   collab host "<Spout source>" [--name <your name>]
//!       start a collab: prints an invite code, then asks for the reply code
//!   collab join "<Spout source>" [--name <your name>]
//!       join a collab: asks for the invite code, prints a reply code
//!
//! Your partner appears in OBS as Spout2 source "Collab - <their name>".

mod console;

use std::sync::Arc;

use collab::{code, ConnState, Connection, Event, EventSink};

const USAGE: &str = "Usage:\n  collab list\n  collab host \"<Spout source>\" [--name <your name>]\n  collab join \"<Spout source>\" [--name <your name>]";

type Error = Box<dyn std::error::Error>;

fn main() {
    collab::quiet_gstreamer_log();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str);
    let sender = args.get(1).filter(|s| !s.starts_with("--"));
    // `--name X` anywhere after the mode; default: the source name.
    let name = args
        .iter()
        .position(|a| a == "--name")
        .and_then(|i| args.get(i + 1))
        .or(sender);

    let result = match (mode, sender, name) {
        (Some("list"), _, _) => list(),
        (Some("host"), Some(sender), Some(name)) => host(sender, name),
        (Some("join"), Some(sender), Some(name)) => join(sender, name),
        _ => Err(USAGE.into()),
    };
    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

fn list() -> Result<(), Error> {
    let names = collab::avatar_sources();
    if names.is_empty() {
        println!("No Spout2 sources found. Turn on Spout2 output in your VTubing app.");
    } else {
        println!("Spout2 sources on this PC:");
        for n in names {
            println!("  \"{n}\"");
        }
    }
    Ok(())
}

/// Turns events into printed lines.
fn print_events() -> EventSink {
    Arc::new(|event| match event {
        Event::Info(text) => println!("{text}"),
        Event::Connection(state) => println!(
            "[connection] {}",
            match state {
                ConnState::Connecting => "connecting...",
                ConnState::Connected => "CONNECTED",
                ConnState::Failed => "FAILED — no direct route found (strict network/firewall?)",
                ConnState::Disconnected => "disconnected",
                ConnState::Closed => "closed",
            }
        ),
        Event::PartnerVideo => println!("[video] partner's stream started"),
        Event::Publishing(name) => println!("[spout] publishing \"{name}\" — add it in OBS with Spout2 Capture"),
        Event::Stats { sent_fps, received_fps } => {
            println!("[stats] sent {sent_fps:.1} fps | received {received_fps:.1} fps")
        }
        Event::Warning(text) => eprintln!("[warning] {text}"),
    })
}

fn host(sender: &str, name: &str) -> Result<(), Error> {
    let (mut conn, invite) = Connection::host(sender, name, print_events())?;
    console::show_code("INVITE", &invite);
    println!("Send this invite code to your partner, then paste their reply code here.");
    loop {
        let text = console::read_line("Reply code: ")?;
        match conn.accept_reply(&text) {
            Ok(partner) => {
                println!("Connecting to {partner}...");
                break;
            }
            Err(e) => println!("Couldn't use that code: {e}. Try again."),
        }
    }
    println!("Press Enter to stop.");
    let stop = console::stop_on_enter();
    conn.run(&stop)?;
    println!("Stopped.");
    Ok(())
}

fn join(sender: &str, name: &str) -> Result<(), Error> {
    // Ask until a valid invite code is pasted.
    let invite = loop {
        let text = console::read_line("Paste the invite code: ")?;
        match code::decode(&text) {
            Ok(_) => break text,
            Err(e) => println!("Couldn't use that code: {e}. Try again."),
        }
    };
    let (conn, reply, partner) = Connection::join(sender, name, &invite, print_events())?;
    console::show_code("REPLY", &reply);
    println!("Send this reply code back to {partner}. Press Enter to stop.");
    let stop = console::stop_on_enter();
    conn.run(&stop)?;
    println!("Stopped.");
    Ok(())
}
