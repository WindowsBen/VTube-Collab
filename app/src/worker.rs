//! Runs a collab connection on a background thread, so the window never freezes.
//!
//! The window and the worker talk through two channels:
//!   window → worker: `ToWorker` (e.g. the pasted reply code)
//!   worker → window: `FromWorker` (codes, connection events, the end)
//! Every message to the window also asks egui to redraw, so it shows up at once.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use collab::{Connection, Event, EventSink};
use eframe::egui;

pub enum ToWorker {
    /// Host only: the partner's reply code, as pasted.
    Reply(String),
}

pub enum FromWorker {
    Event(Event),
    /// Host: our invite code is ready.
    Invite(String),
    /// Host: the pasted reply code was wrong (message says why).
    ReplyRejected(String),
    /// Host: reply accepted; connecting to this partner.
    Accepted(String),
    /// Guest: our reply code is ready to send back to this partner.
    Reply { code: String, partner: String },
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
            send(FromWorker::Invite(invite));
            // Wait for a valid reply code (checking for "stop" every 100 ms).
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
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => return Ok(()),
                }
            }
            conn.run(&stop)
        })
    }

    pub fn join(source: String, name: String, invite: String, ctx: egui::Context) -> Worker {
        Self::spawn(ctx, move |send, events, _inbox, stop| {
            let (conn, code, partner) = Connection::join(&source, &name, &invite, events)?;
            send(FromWorker::Reply { code, partner });
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
