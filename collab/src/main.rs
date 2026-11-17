//! VTube Collab command-line tool (the app window is in `app/`).
//!
//!   collab list
//!       show the Spout2 sources on this PC (your avatar should be one of them)
//!   collab host "<Spout source>" [--name <your name>] [--long]
//!       start a collab: prints a short room code (or, with --long or if the room
//!       server is unreachable, a long invite code and asks for the reply code)
//!   collab join "<Spout source>" [--name <your name>]
//!       join a collab: asks for the room code (or a long invite code)
//!
//! Your partner appears in OBS as Spout2 source "Collab - <their name>".

mod console;

use std::sync::Arc;
use std::time::Duration;

use collab::rooms::{self, RoomError};
use collab::{code, ConnState, Connection, Event, EventSink};

const USAGE: &str = "Usage:\n  collab list\n  collab host \"<Spout source>\" [--name <your name>] [--long]\n  collab join \"<Spout source>\" [--name <your name>]\n\n--long: use copy-paste long codes instead of a short room code.";

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

    let long_codes = args.iter().any(|a| a == "--long");

    let result = match (mode, sender, name) {
        (Some("list"), _, _) => list(),
        (Some("host"), Some(sender), Some(name)) => host(sender, name, long_codes),
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

fn host(sender: &str, name: &str, long_codes: bool) -> Result<(), Error> {
    let (mut conn, invite) = Connection::host(sender, name, print_events())?;

    // Normally: a short room code, and the reply arrives through the room server.
    if !long_codes {
        match rooms::create(&invite) {
            Ok(room) => {
                println!("\nRoom code (send this to your partner):");
                println!("CODE: {room}");
                println!("Waiting for your partner to join (room codes last 10 minutes)...");
                loop {
                    match rooms::poll_reply(&room) {
                        Ok(Some(reply)) => {
                            let partner = conn.accept_reply(&reply)?;
                            println!("{partner} joined. Connecting...");
                            break;
                        }
                        Ok(None) => std::thread::sleep(Duration::from_secs(1)),
                        Err(RoomError::NotFound) => {
                            return Err("the room code expired before anyone joined. Run host again for a new one.".into());
                        }
                        Err(e) => return Err(e.into()),
                    }
                }
                return run_until_enter(conn);
            }
            Err(e) => println!("Room codes unavailable ({e}). Using long codes instead."),
        }
    }

    // Fallback: copy-paste long codes.
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
    run_until_enter(conn)
}

fn join(sender: &str, name: &str) -> Result<(), Error> {
    // Ask until a valid room code or long invite code is entered.
    let (invite, room) = loop {
        let text = console::read_line("Room code or invite code: ")?;
        if code::decode(&text).is_ok() {
            break (text, None);
        }
        let Some(room) = rooms::normalise(&text) else {
            println!("That isn't a room code (like MINTY-OTTER-42) or an invite code (VTC1-...). Try again.");
            continue;
        };
        match rooms::take_invite(&room) {
            Ok(invite) => break (invite, Some(room)),
            Err(e) => println!("Couldn't join: {e}. Try again."),
        }
    };

    let (conn, reply, partner) = Connection::join(sender, name, &invite, print_events())?;
    match room {
        Some(room) => {
            rooms::put_reply(&room, &reply)?;
            println!("Joined {partner}'s room. Connecting...");
        }
        None => {
            console::show_code("REPLY", &reply);
            println!("Send this reply code back to {partner}.");
        }
    }
    run_until_enter(conn)
}

fn run_until_enter(conn: Connection) -> Result<(), Error> {
    println!("Press Enter to stop.");
    let stop = console::stop_on_enter();
    conn.run(&stop)?;
    println!("Stopped.");
    Ok(())
}
