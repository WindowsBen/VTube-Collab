//! Runs a collab connection on a background thread, so the window never freezes.
//!
//! The window and the worker talk through two channels:
//!   window → worker: `ToWorker` (e.g. a pasted long reply code)
//!   worker → window: `FromWorker` (codes, connection events, the end)
//! Every message to the window also asks egui to redraw, so it shows up at once.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use collab::rooms::{self, RoomError};
use collab::{code, Connection, Event, EventSink};
use eframe::egui;

/// How often the host checks the room for the guest's reply.
const POLL_EVERY: Duration = Duration::from_secs(1);

pub enum ToWorker {
    /// Host, long-code mode: the partner's reply code, as pasted.
    Reply(String),
}

pub enum FromWorker {
    Event(Event),
    /// Host: short room code ready.
    RoomCode(String),
    /// Host: room codes can't be used right now (why); long codes still work.
    RoomUnavailable(String),
    /// Host: nobody joined before the room code expired.
    RoomExpired,
    /// Host: our long invite code (fallback).
    Invite(String),
    /// Host: a pasted long reply code was wrong (message says why).
    ReplyRejected(String),
    /// Host: a partner joined; connecting.
    Accepted(String),
    /// Guest (long codes): our reply code is ready to send back to this partner.
    Reply { code: String, partner: String },
    /// Guest (room code): reply delivered automatically; connecting to this partner.
    ReplySent(String),
    /// The connection ended (`Some(message)` if because of an error).
    Ended(Option<String>),
}

pub struct Worker {
    to_worker: Sender<ToWorker>,
    pub from_worker: Receiver<FromWorker>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn host(source: String, name: String, ctx: egui::Context) -> Worker {
        Self::spawn(ctx, move |send, events, inbox, stop| {
            let (mut conn, invite) = Connection::host(&source, &name, events)?;

            // A short room code if the server is reachable; the long code always works too.
            let mut room = match rooms::create(&invite) {
                Ok(code) => {
                    send(FromWorker::RoomCode(code.clone()));
                    Some(code)
                }
                Err(e) => {
                    send(FromWorker::RoomUnavailable(e.to_string()));
                    None
                }
            };
            send(FromWorker::Invite(invite));

            // Wait for the partner: through the room, or a pasted long reply code.
            let mut last_poll = Instant::now() - POLL_EVERY;
            loop {
                if stop.load(Ordering::Relaxed) {
                    return Ok(());
                }
                match inbox.recv_timeout(Duration::from_millis(100)) {
                    Ok(ToWorker::Reply(text)) => match conn.accept_reply(&text) {
                        Ok(partner) => {
                            send(FromWorker::Accepted(partner));
                            break;
                        }
                        Err(e) => send(FromWorker::ReplyRejected(e.to_string())),
                    },
                    Err(RecvTimeoutError::Disconnected) => return Ok(()),
                    Err(RecvTimeoutError::Timeout) => {}
                }
                if let Some(code) = &room {
                    if last_poll.elapsed() >= POLL_EVERY {
                        last_poll = Instant::now();
                        match rooms::poll_reply(code) {
                            Ok(Some(reply)) => {
                                let partner = conn.accept_reply(&reply)?;
                                send(FromWorker::Accepted(partner));
                                break;
                            }
                            Ok(None) => {}
                            Err(RoomError::NotFound) => {
                                send(FromWorker::RoomExpired);
                                room = None;
                            }
                            // A network hiccup: just try again next second.
                            Err(_) => {}
                        }
                    }
                }
            }
            conn.run(&stop)
        })
    }

    /// `input` = a room code (MINTY-OTTER-42) or a long invite code (VTC1-...).
    pub fn join(source: String, name: String, input: String, ctx: egui::Context) -> Worker {
        Self::spawn(ctx, move |send, events, _inbox, stop| {
            let room = if code::decode(&input).is_ok() { None } else { rooms::normalise(&input) };
            let invite = match &room {
                Some(room) => rooms::take_invite(room)?,
                None => input,
            };
            let (conn, reply, partner) = Connection::join(&source, &name, &invite, events)?;
            match &room {
                Some(room) => {
                    rooms::put_reply(room, &reply)?;
                    send(FromWorker::ReplySent(partner));
                }
                None => send(FromWorker::Reply { code: reply, partner }),
            }
            conn.run(&stop)
        })
    }

    /// Start the thread running `job`, and report how it ended.
    fn spawn(
        ctx: egui::Context,
        job: impl FnOnce(
            &dyn Fn(FromWorker),
            EventSink,
            &Receiver<ToWorker>,
            &AtomicBool,
        ) -> Result<(), collab::Error>
            + Send
            + 'static,
    ) -> Worker {
        let (to_worker, inbox) = mpsc::channel();
        let (outbox, from_worker) = mpsc::channel::<FromWorker>();
        let stop = Arc::new(AtomicBool::new(false));

        let thread_stop = stop.clone();
        let thread = std::thread::spawn(move || {
            // `send` = deliver a message to the window and ask it to redraw.
            let send = {
                let outbox = outbox.clone();
                let ctx = ctx.clone();
                move |msg: FromWorker| {
                    let _ = outbox.send(msg);
                    ctx.request_repaint();
                }
            };
            // Connection events go to the window the same way. A Mutex makes the
            // channel end shareable, since events can come from any thread.
            let events: EventSink = {
                let outbox = std::sync::Mutex::new(outbox.clone());
                let ctx = ctx.clone();
                Arc::new(move |e| {
                    let _ = outbox.lock().unwrap().send(FromWorker::Event(e));
                    ctx.request_repaint();
                })
            };
            let result = job(&send, events, &inbox, &thread_stop);
            send(FromWorker::Ended(result.err().map(|e| e.to_string())));
        });

        Worker { to_worker, from_worker, stop, thread: Some(thread) }
    }

    pub fn send(&self, msg: ToWorker) {
        let _ = self.to_worker.send(msg);
    }
}

impl Drop for Worker {
    /// Leaving / closing the window: stop the connection and wait for clean-up
    /// (this removes our partner's Spout source from OBS's list).
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
