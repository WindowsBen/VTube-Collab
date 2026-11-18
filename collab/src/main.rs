//! VTube Collab command-line tool (the app window is in `app/`). Mostly for testing.
//!
//!   collab list
//!       show the Spout2 sources on this PC (your avatar should be one of them)
//!   collab host "<Spout source>" [--name <your name>] [--max-people N] [--buffer MS] [--group] [--long]
//!       create a room and print its code. Everyone who asks to join is let in
//!       automatically (the app asks you first). --max-people: 2–100 (default 20),
//!       --buffer: receive buffer 20–500 ms (default 120). With --long: a two-person
//!       collab with copy-paste long codes instead (no room server needed).
//!   collab join "<Spout source>" [--name <your name>] [--group]
//!       join: asks for a room code (or a long invite code)
//!
//! Partners appear in OBS as Spout2 sources "Collab - <their name>", and everyone
//! together as "Collab - Group".

mod console;

use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use collab::{code, ConnState, Connection, Event, EventSink, Room, RoomCommand, RoomEvent, RoomSettings};

const USAGE: &str = "Usage:\n  collab list\n  collab host \"<Spout source>\" [--name <your name>] [--max-people N] [--buffer MS] [--group] [--long]\n  collab join \"<Spout source>\" [--name <your name>] [--group]\n\n--group: also publish the group source \"Collab - Group\" (everyone in one OBS source).\n--max-people: 2-100 (default 20). --buffer: receive buffer in ms, 20-500 (default 120).\n--long: a two-person collab with copy-paste long codes instead of a room code.";

type Error = Box<dyn std::error::Error>;

fn main() {
    collab::log::start("cli");
    collab::quiet_gstreamer_log();
    if let Some(path) = collab::log::path() {
        println!("Log file: {}", path.display());
    }

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
    // `--flag N` → N as a number, if given.
    let number = |flag: &str| {
        args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).and_then(|v| v.parse::<u32>().ok())
    };
    let settings = RoomSettings {
        max_people: number("--max-people").unwrap_or(collab::DEFAULT_PEOPLE).clamp(collab::MIN_PEOPLE, collab::MAX_PEOPLE),
        buffer_ms: number("--buffer").unwrap_or(collab::DEFAULT_BUFFER_MS).clamp(collab::MIN_BUFFER_MS, collab::MAX_BUFFER_MS),
    };

    let result = match (mode, sender, name) {
        (Some("list"), _, _) => list(),
        (Some("host"), Some(sender), Some(name)) if long_codes => host_long(sender, name),
        (Some("host"), Some(sender), Some(name)) => host_room(sender, name, settings),
        (Some("join"), Some(sender), Some(name)) => join(sender, name),
        _ => Err(USAGE.into()),
    };
    if let Err(e) = result {
        collab::log::error(&e.to_string());
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

/// Turns connection events into printed lines.
fn print_events() -> EventSink {
    Arc::new(|event| {
        collab::log::event(&event);
        print_event(event)
    })
}

fn print_event(event: Event) {
    match event {
        Event::Info(text) => println!("{text}"),
        Event::Connection { peer, state } => println!(
            "[connection {peer}] {}",
            match state {
                ConnState::Connecting => "connecting...",
                ConnState::Connected => "CONNECTED",
                ConnState::Failed => "FAILED — no direct route found (strict network/firewall?)",
                ConnState::Disconnected => "disconnected",
                ConnState::Closed => "closed",
            }
        ),
        Event::PartnerVideo { peer } => println!("[video {peer}] stream started"),
        Event::Publishing { source, .. } => println!("[spout] publishing \"{source}\" — add it in OBS with Spout2 Capture"),
        Event::GroupPublishing(name) => println!("[spout] publishing group source \"{name}\" (everyone stacked)"),
        Event::GroupStopped => println!("[spout] group source switched off"),
        Event::Stats { sent_fps, received } => {
            let from: Vec<String> = received
                .iter()
                .map(|p| {
                    let loss = p.loss_percent.map(|l| format!(" loss {l:.1}%")).unwrap_or_default();
                    let loss = format!("{loss}{}", p.unrepaired.map(|u| format!(" unrepaired {u}")).unwrap_or_default());
                    let rtt = p.round_trip_ms.map(|r| format!(" rtt {r:.0}ms")).unwrap_or_default();
                    format!("{} {:.1}{loss}{rtt}", p.name, p.fps)
                })
                .collect();
            println!("[stats] sent {sent_fps:.1} fps | received: {}", if from.is_empty() { "-".into() } else { from.join(", ") });
        }
        Event::Warning(text) => eprintln!("[warning] {text}"),
    }
}

/// Turns room events into printed lines; the host lets everyone in automatically.
fn print_room_events(approve: Option<mpsc::Sender<RoomCommand>>) -> Arc<dyn Fn(RoomEvent) + Send + Sync> {
    let approve = Mutex::new(approve);
    Arc::new(move |event| {
        collab::log::info(&format!("Room: {event:?}"));
        print_room_event(event, &approve)
    })
}

fn print_room_event(event: RoomEvent, approve: &Mutex<Option<mpsc::Sender<RoomCommand>>>) {
    match event {
        RoomEvent::Waiting => println!("[room] waiting for the host to let you in..."),
        RoomEvent::Admitted { host, settings } => println!(
            "[room] you're in{} — up to {} people, {} ms buffer",
            if host { " (host)" } else { "" },
            settings.max_people,
            settings.buffer_ms
        ),
        RoomEvent::JoinRequest { id, name } => {
            println!("[room] {name} asks to join — letting them in");
            if let Some(tx) = approve.lock().unwrap().as_ref() {
                let _ = tx.send(RoomCommand::Approve(id));
            }
        }
        RoomEvent::JoinCancelled { .. } => println!("[room] someone stopped waiting to join"),
        RoomEvent::MemberJoined { id, name } => {
            println!("[room] {name} is in the room");
            // TEST ONLY: VTC_TEST_REMOVE=<name> makes the host remove that person 10 s after they join.
            if std::env::var("VTC_TEST_REMOVE").is_ok_and(|n| n == name) {
                if let Some(tx) = approve.lock().unwrap().clone() {
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_secs(10));
                        println!("[room] removing {name} (test)");
                        let _ = tx.send(RoomCommand::Remove(id));
                    });
                }
            }
            // TEST ONLY: VTC_TEST_BUFFER=<ms> makes the host change the receive buffer 10 s after someone joins.
            if let Some(ms) = std::env::var("VTC_TEST_BUFFER").ok().and_then(|v| v.parse::<u32>().ok()) {
                if let Some(tx) = approve.lock().unwrap().clone() {
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_secs(10));
                        println!("[room] setting the receive buffer to {ms} ms (test)");
                        let _ = tx.send(RoomCommand::SetBuffer(ms));
                    });
                }
            }
        }
        RoomEvent::SettingsChanged { settings } => println!("[room] receive buffer is now {} ms", settings.buffer_ms),
        RoomEvent::MemberLeft { name, .. } => println!("[room] {name} left"),
        RoomEvent::BecameHost => println!("[room] the host left — you're the host now"),
        RoomEvent::ServerReconnecting => eprintln!("[room] room server connection dropped; reconnecting..."),
        RoomEvent::ServerBack { host } => {
            println!("[room] reconnected to the room server{}", if host { " (host)" } else { "" })
        }
        RoomEvent::ServerLost => eprintln!("[room] lost the room server; nobody new can join"),
    }
}

fn host_room(sender: &str, name: &str, settings: RoomSettings) -> Result<(), Error> {
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (room, code) = Room::host(sender, name, settings, print_events(), print_room_events(Some(cmd_tx)))?;
    println!("\nRoom code (send this to everyone joining):");
    println!("CODE: {code}");
    println!("Press Enter to stop.");
    let stop = console::stop_on_enter();
    room.group_enabled().store(wants_group(), Ordering::Relaxed);
    room.run(&stop, cmd_rx)?;
    println!("Stopped.");
    Ok(())
}

/// Two-person collab with copy-paste long codes.
fn host_long(sender: &str, name: &str) -> Result<(), Error> {
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
    conn.group_enabled().store(wants_group(), Ordering::Relaxed);
    conn.run(&stop)?;
    println!("Stopped.");
    Ok(())
}

fn join(sender: &str, name: &str) -> Result<(), Error> {
    let text = loop {
        let text = console::read_line("Room code or invite code: ")?;
        if code::decode(&text).is_ok() || collab::rooms::normalise(&text).is_some() {
            break text;
        }
        println!("That isn't a room code (like MINTY-OTTER-42) or an invite code (VTC1-...). Try again.");
    };

    if code::decode(&text).is_ok() {
        // Long codes: two-person collab, reply sent back by hand.
        let (conn, reply, partner) = Connection::join(sender, name, &text, print_events())?;
        console::show_code("REPLY", &reply);
        println!("Send this reply code back to {partner}. Press Enter to stop.");
        let stop = console::stop_on_enter();
        conn.group_enabled().store(wants_group(), Ordering::Relaxed);
    conn.run(&stop)?;
    } else {
        let room = Room::join(sender, name, &text, print_events(), print_room_events(None))?;
        println!("Press Enter to stop.");
        let stop = console::stop_on_enter();
        let (_unused_tx, cmd_rx) = mpsc::channel();
        room.group_enabled().store(wants_group(), Ordering::Relaxed);
    room.run(&stop, cmd_rx)?;
    }
    println!("Stopped.");
    Ok(())
}

/// `--group` given: also publish the group source.
fn wants_group() -> bool {
    std::env::args().any(|a| a == "--group")
}
