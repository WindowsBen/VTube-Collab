//! Runs a collab on a background thread, so the window never freezes.
//!
//! The window and the worker talk through two channels:
//!   window → worker: `ToWorker` (approve/decline someone, a pasted long reply code)
//!   worker → window: `FromWorker` (room code, room/connection events, the end)
//! Every message to the window also asks egui to redraw, so it shows up at once.
//!
//! Two kinds of collab:
//!   * a room (normal): 2–20 people, one room code, the host approves who joins
//!   * long codes (fallback, two people): copy-paste codes, no room server needed

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use collab::{Connection, Event, EventSink, PeerId, Room, RoomCommand, RoomEvent, RoomSettings};
use eframe::egui;

pub enum ToWorker {
    Approve(PeerId),
    Decline(PeerId),
    /// Long-code host: the partner's reply code, as pasted.
    Reply(String),
}

pub enum FromWorker {
    Event(Event),
    Room(RoomEvent),
    /// Host: the room is open; share this code.
    RoomCode(String),
    /// Host: rooms can't be used right now (why); fall back to long codes.
    RoomUnavailable(String),
    /// Long-code host: our invite code.
    Invite(String),
    /// Long-code host: a pasted reply code was wrong (message says why).
    ReplyRejected(String),
    /// Long-code host: the partner's reply was accepted; connecting.
    Accepted(String),
    /// Long-code guest: our reply code, to send back to this partner.
    Reply { code: String, partner: String },
    /// The group source's layer list and on/off switch, for the group panel.
    Group(collab::GroupLayers, std::sync::Arc<std::sync::atomic::AtomicBool>),
    /// The collab ended (`Some(message)` if because of an error).
    Ended(Option<String>),
}

pub struct Worker {
    to_worker: Sender<ToWorker>,
    pub from_worker: Receiver<FromWorker>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// What a worker job gets to work with.
struct Ctx<'a> {
    send: &'a dyn Fn(FromWorker),
    events: EventSink,
    room_events: Arc<dyn Fn(RoomEvent) + Send + Sync>,
    inbox: Receiver<ToWorker>,
    stop: &'a AtomicBool,
}

impl Worker {
    /// Open a room with `settings`. If the room server can't be reached, falls back to long codes.
    pub fn host(source: String, name: String, settings: RoomSettings, ctx: egui::Context) -> Worker {
        Self::spawn(ctx, move |c| {
            match Room::host(&source, &name, settings, c.events.clone(), c.room_events.clone()) {
                Ok((room, code)) => {
                    (c.send)(FromWorker::RoomCode(code));
                    (c.send)(FromWorker::Group(room.group_layers(), room.group_enabled()));
                    room.run(c.stop, forward_commands(c.inbox))
                }
                Err(e) => {
                    (c.send)(FromWorker::RoomUnavailable(e.to_string()));
                    host_long_codes(&source, &name, c)
                }
            }
        })
    }

    /// Join with a room code, or a long invite code (VTC1-...).
    pub fn join(source: String, name: String, input: String, ctx: egui::Context) -> Worker {
        Self::spawn(ctx, move |c| {
            if collab::code::decode(&input).is_ok() {
                let (conn, code, partner) = Connection::join(&source, &name, &input, c.events.clone())?;
                (c.send)(FromWorker::Reply { code, partner });
                (c.send)(FromWorker::Group(conn.group_layers(), conn.group_enabled()));
                conn.run(c.stop)
            } else {
                let room = Room::join(&source, &name, &input, c.events.clone(), c.room_events.clone())?;
                (c.send)(FromWorker::Group(room.group_layers(), room.group_enabled()));
                room.run(c.stop, forward_commands(c.inbox))
            }
        })
    }

    /// Start the thread running `job`, and report how it ended.
    fn spawn(
        egui_ctx: egui::Context,
        job: impl FnOnce(Ctx) -> Result<(), collab::Error> + Send + 'static,
    ) -> Worker {
        let (to_worker, inbox) = mpsc::channel();
        let (outbox, from_worker) = mpsc::channel::<FromWorker>();
        let stop = Arc::new(AtomicBool::new(false));

        let thread_stop = stop.clone();
        let thread = std::thread::spawn(move || {
            // Every message to the window also asks it to redraw. A Mutex makes the
            // channel end shareable, since events can come from any thread.
            let outbox = Arc::new(Mutex::new(outbox));
            let deliver = {
                let (outbox, egui_ctx) = (outbox.clone(), egui_ctx.clone());
                move |msg: FromWorker| {
                    let _ = outbox.lock().unwrap().send(msg);
                    egui_ctx.request_repaint();
                }
            };
            let events: EventSink = {
                let deliver = deliver.clone();
                Arc::new(move |e| deliver(FromWorker::Event(e)))
            };
            let room_events: Arc<dyn Fn(RoomEvent) + Send + Sync> = {
                let deliver = deliver.clone();
                Arc::new(move |e| deliver(FromWorker::Room(e)))
            };
            let result = job(Ctx { send: &deliver, events, room_events, inbox, stop: &thread_stop });
            deliver(FromWorker::Ended(result.err().map(|e| e.to_string())));
        });

        Worker { to_worker, from_worker, stop, thread: Some(thread) }
    }

    pub fn send(&self, msg: ToWorker) {
        let _ = self.to_worker.send(msg);
    }
}

/// Pass the window's Accept/Decline clicks on to the room.
fn forward_commands(inbox: Receiver<ToWorker>) -> Receiver<RoomCommand> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        // Ends when the window's side of the channel is dropped (collab over).
        for msg in inbox {
            let cmd = match msg {
                ToWorker::Approve(id) => RoomCommand::Approve(id),
                ToWorker::Decline(id) => RoomCommand::Decline(id),
                ToWorker::Reply(_) => continue,
            };
            if tx.send(cmd).is_err() {
                break;
            }
        }
    });
    rx
}

/// Two-person fallback: show a long invite code, wait for a pasted reply code.
fn host_long_codes(source: &str, name: &str, c: Ctx) -> Result<(), collab::Error> {
    let (mut conn, invite) = Connection::host(source, name, c.events.clone())?;
    (c.send)(FromWorker::Invite(invite));
    loop {
        if c.stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        match c.inbox.recv_timeout(Duration::from_millis(100)) {
            Ok(ToWorker::Reply(text)) => match conn.accept_reply(&text) {
                Ok(partner) => {
                    (c.send)(FromWorker::Accepted(partner));
                    break;
                }
                Err(e) => (c.send)(FromWorker::ReplyRejected(e.to_string())),
            },
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
    (c.send)(FromWorker::Group(conn.group_layers(), conn.group_enabled()));
    conn.run(c.stop)
}

impl Drop for Worker {
    /// Leaving / closing the window: stop the collab and wait for clean-up
    /// (this removes our Spout sources from OBS's list).
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
