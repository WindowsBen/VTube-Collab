//! VTube Collab — the app window.
//!
//! Screens: Setup → (Host | Join) → Live. The connection itself runs on a
//! background thread (worker.rs); this file only draws and reacts to clicks.

// Release builds open without a black console window behind the app.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod theme;
mod worker;
mod xp;

use std::time::{Duration, Instant};

use collab::{ConnState, Event};
use eframe::egui::{self, Color32, Rect, RichText, UiBuilder, pos2, vec2};

use theme::colour;
use worker::{FromWorker, ToWorker, Worker};

const WINDOW_SIZE: [f32; 2] = [480.0, 440.0];
const STATUS_BAR_HEIGHT: f32 = 22.0;

fn main() -> eframe::Result {
    collab::quiet_gstreamer_log();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("VTube Collab")
            .with_inner_size(WINDOW_SIZE)
            .with_resizable(false)
            .with_decorations(false) // we draw our own XP title bar
            .with_transparent(true), // so the rounded top corners can be see-through
        ..Default::default()
    };
    eframe::run_native("VTube Collab", options, Box::new(|cc| Ok(Box::new(App::new(&cc.egui_ctx)))))
}

/// The host's short room code, as far as we know.
enum Room {
    Preparing,
    Ready(String),
    Expired,
    Unavailable(String),
}

/// Which screen is showing, and what it needs to remember.
enum Screen {
    Setup,
    Host {
        room: Room,
        /// Long-code fallback.
        invite: Option<String>,
        show_long: bool,
        reply_input: String,
        error: Option<String>,
    },
    Join {
        input: String,
        /// Long-code mode: (our reply code, partner) to send back by hand.
        reply: Option<(String, String)>,
        /// Room mode: reply delivered; waiting to connect to this partner.
        waiting_for: Option<String>,
        error: Option<String>,
    },
    Live { partner: String },
}

impl Screen {
    fn new_host() -> Screen {
        Screen::Host { room: Room::Preparing, invite: None, show_long: false, reply_input: String::new(), error: None }
    }
}

struct App {
    name: String,
    sources: Vec<String>,
    source: Option<String>,
    last_refresh: Instant,
    screen: Screen,
    worker: Option<Worker>,
    // What the connection has told us so far.
    status: String,
    connection: Option<ConnState>,
    publishing: Option<String>,
    stats: Option<(f64, f64)>,
    copied_at: Option<Instant>,
    setup_error: Option<String>,
}

impl App {
    fn new(ctx: &egui::Context) -> App {
        theme::apply(ctx);
        let sources = collab::avatar_sources();
        App {
            name: String::new(),
            source: sources.first().cloned(),
            sources,
            last_refresh: Instant::now(),
            screen: Screen::Setup,
            worker: None,
            status: "Ready.".into(),
            connection: None,
            publishing: None,
            stats: None,
            copied_at: None,
            setup_error: None,
        }
    }

    /// Back to the start (stops any connection; dropping the worker cleans up).
    fn reset(&mut self, status: impl Into<String>) {
        self.worker = None;
        self.screen = Screen::Setup;
        self.status = status.into();
        self.clear_connection_info();
    }

    fn clear_connection_info(&mut self) {
        self.connection = None;
        self.publishing = None;
        self.stats = None;
    }

    /// (Re)start hosting: a fresh connection and a fresh room code.
    fn start_hosting(&mut self, ctx: &egui::Context) {
        self.worker = None; // stop any previous attempt first
        self.clear_connection_info();
        self.worker = Some(Worker::host(self.source.clone().unwrap(), self.name.trim().into(), ctx.clone()));
        self.screen = Screen::new_host();
        self.setup_error = None;
        self.status = "Starting...".into();
    }

    fn refresh_sources(&mut self) {
        self.sources = collab::avatar_sources();
        if self.source.as_ref().is_none_or(|s| !self.sources.contains(s)) {
            self.source = self.sources.first().cloned();
        }
        self.last_refresh = Instant::now();
    }

    /// Handle everything the background connection has reported since last frame.
    fn handle_worker_messages(&mut self) {
        let Some(worker) = &self.worker else { return };
        let messages: Vec<FromWorker> = worker.from_worker.try_iter().collect();
        for msg in messages {
            match msg {
                FromWorker::Event(e) => self.handle_event(e),
                FromWorker::RoomCode(code) => {
                    if let Screen::Host { room, .. } = &mut self.screen {
                        *room = Room::Ready(code);
                    }
                    self.status = "Room code ready. Send it to your partner.".into();
                }
                FromWorker::RoomUnavailable(why) => {
                    if let Screen::Host { room, show_long, .. } = &mut self.screen {
                        *room = Room::Unavailable(why);
                        *show_long = true;
                    }
                    self.status = "Room codes unavailable; use the long code instead.".into();
                }
                FromWorker::RoomExpired => {
                    if let Screen::Host { room, .. } = &mut self.screen {
                        *room = Room::Expired;
                    }
                    self.status = "The room code expired.".into();
                }
                FromWorker::Invite(code) => {
                    if let Screen::Host { invite, .. } = &mut self.screen {
                        *invite = Some(code);
                    }
                }
                FromWorker::ReplyRejected(e) => {
                    if let Screen::Host { error, .. } = &mut self.screen {
                        *error = Some(format!("That code didn't work: {e}"));
                    }
                }
                FromWorker::Accepted(partner) => {
                    self.status = format!("{partner} joined. Connecting...");
                    self.screen = Screen::Live { partner };
                }
                FromWorker::Reply { code, partner } => {
                    if let Screen::Join { reply, .. } = &mut self.screen {
                        *reply = Some((code, partner.clone()));
                    }
                    self.status = format!("Reply code ready. Send it back to {partner}.");
                }
                FromWorker::ReplySent(partner) => {
                    if let Screen::Join { waiting_for, .. } = &mut self.screen {
                        *waiting_for = Some(partner.clone());
                    }
                    self.status = format!("Joined {partner}'s room. Connecting...");
                }
                FromWorker::Ended(error) => {
                    self.worker = None;
                    // Joining failed before connecting (e.g. wrong room code): stay on the
                    // Join screen so the code can be fixed. Anything else: back to Setup.
                    if let (Screen::Join { error: join_error, waiting_for: None, reply: None, .. }, Some(e)) =
                        (&mut self.screen, &error)
                    {
                        *join_error = Some(format!("Couldn't join: {e}"));
                        self.status = "Couldn't join.".into();
                        return;
                    }
                    self.setup_error = error.as_ref().map(|e| format!("Stopped: {e}"));
                    self.reset(self.setup_error.clone().unwrap_or_else(|| "Collab ended.".into()));
                    return;
                }
            }
        }
    }

    fn handle_event(&mut self, e: Event) {
        match e {
            Event::Info(text) => self.status = text,
            Event::Connection(state) => {
                // The guest moves to the Live screen once the connection is up.
                if state == ConnState::Connected {
                    let partner = match &self.screen {
                        Screen::Join { reply: Some((_, p)), .. } | Screen::Join { waiting_for: Some(p), .. } => {
                            Some(p.clone())
                        }
                        _ => None,
                    };
                    if let Some(partner) = partner {
                        self.screen = Screen::Live { partner };
                    }
                }
                self.status = match state {
                    ConnState::Connecting => "Connecting...".into(),
                    ConnState::Connected => "Connected.".into(),
                    ConnState::Failed => "Connection failed: no direct route between your PCs.".into(),
                    ConnState::Disconnected => "Connection lost. Waiting for it to come back...".into(),
                    ConnState::Closed => "Connection closed.".into(),
                };
                self.connection = Some(state);
            }
            Event::PartnerVideo => self.status = "Receiving your partner's avatar.".into(),
            Event::Publishing(name) => self.publishing = Some(name),
            Event::Stats { sent_fps, received_fps } => self.stats = Some((sent_fps, received_fps)),
            Event::Warning(text) => self.status = format!("Warning: {text}"),
        }
    }

    // ---------------- screens ----------------

    fn setup_screen(&mut self, ui: &mut egui::Ui) {
        // Re-check the Spout sources every 2 s while on this screen.
        if self.last_refresh.elapsed() > Duration::from_secs(2) {
            self.refresh_sources();
        }
        ui.ctx().request_repaint_after(Duration::from_secs(2));

        xp::group(ui, "Your setup", |ui| {
            egui::Grid::new("setup").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
                ui.label("Your name:");
                xp::text_box(ui, &mut self.name, "How your partner will see you", 250.0);
                ui.end_row();

                ui.label("Your avatar:");
                let selected = self.source.clone().unwrap_or_else(|| "(no Spout2 sources found)".into());
                egui::ComboBox::from_id_salt("avatar").selected_text(selected).width(250.0).show_ui(ui, |ui| {
                    for s in &self.sources {
                        ui.selectable_value(&mut self.source, Some(s.clone()), s);
                    }
                });
                ui.end_row();
            });
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    "Your avatar is your VTubing app's Spout2 output. If the list is empty, \
                     turn on Spout2 output in your VTubing app.",
                )
                .size(11.0)
                .color(Color32::from_rgb(0x44, 0x44, 0x44)),
            );
        });

        ui.add_space(10.0);
        let ready = !self.name.trim().is_empty() && self.source.is_some();
        ui.horizontal(|ui| {
            if xp::button(ui, "Host a collab", ready, true).clicked() {
                self.start_hosting(ui.ctx());
            }
            if xp::button(ui, "Join a collab", ready, false).clicked() {
                self.setup_error = None;
                self.screen = Screen::Join { input: String::new(), reply: None, waiting_for: None, error: None };
                self.status = "Enter the room code from your partner.".into();
            }
        });
        if !ready {
            ui.label(RichText::new("Enter your name and pick your avatar to start.").size(11.0).color(colour::TEXT_DISABLED));
        }
        if let Some(e) = &self.setup_error {
            ui.add_space(4.0);
            ui.colored_label(colour::ERROR, e);
        }
    }

    fn host_screen(&mut self, ui: &mut egui::Ui) {
        let mut new_code = false;
        let mut send_reply: Option<String> = None;
        let Screen::Host { room, invite, show_long, reply_input, error } = &mut self.screen else { return };

        xp::group(ui, "Send this room code to your partner", |ui| match room {
            Room::Preparing => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Getting a room code...");
                });
            }
            Room::Ready(code) => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(code.as_str()).font(theme::bold(20.0)).color(colour::GROUP_TITLE));
                    ui.add_space(8.0);
                    copy_button(ui, code, &mut self.copied_at);
                });
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Waiting for your partner to join...");
                });
                ui.label(RichText::new("Room codes last 10 minutes. You can get a new one any time.").size(11.0).color(colour::TEXT_DISABLED));
            }
            Room::Expired => {
                ui.colored_label(colour::ERROR, "This room code expired before anyone joined.");
                ui.add_space(4.0);
                new_code = xp::button(ui, "Get new room code", true, true).clicked();
            }
            Room::Unavailable(why) => {
                ui.colored_label(colour::ERROR, "Room codes are unavailable right now.");
                ui.label(RichText::new(format!("({why})")).size(11.0).color(colour::TEXT_DISABLED));
                ui.label("Use the long code below instead.");
            }
        });
        ui.add_space(6.0);

        // Long-code fallback, folded away unless needed.
        let header = if *show_long { "▼ Use long codes instead" } else { "▶ Use long codes instead" };
        if ui.add(egui::Label::new(RichText::new(header).color(colour::GROUP_TITLE)).sense(egui::Sense::click())).clicked() {
            *show_long = !*show_long;
        }
        if *show_long {
            xp::group(ui, "Long codes (if room codes don't work)", |ui| {
                match invite {
                    Some(code) => {
                        ui.label("1. Send this invite code to your partner:");
                        code_preview(ui, code, &mut self.copied_at);
                    }
                    None => {
                        ui.label("Preparing the invite code...");
                    }
                }
                ui.label("2. Paste their reply code:");
                ui.horizontal(|ui| {
                    let resp = xp::text_box(ui, reply_input, "VTC1-...", 300.0);
                    let enabled = invite.is_some() && !reply_input.trim().is_empty();
                    let go = xp::button(ui, "Connect", enabled, false).clicked()
                        || (enabled && resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                    if go {
                        send_reply = Some(reply_input.trim().to_string());
                    }
                });
                if let Some(e) = error {
                    ui.colored_label(colour::ERROR, e.as_str());
                }
            });
        }

        if let Some(reply) = send_reply {
            *error = None;
            if let Some(w) = &self.worker {
                w.send(ToWorker::Reply(reply));
            }
        }

        ui.add_space(10.0);
        if xp::button(ui, "Cancel", true, false).clicked() {
            self.reset("Cancelled.");
        } else if new_code {
            self.start_hosting(ui.ctx());
        }
    }

    fn join_screen(&mut self, ui: &mut egui::Ui) {
        let Screen::Join { input, reply, waiting_for, error } = &mut self.screen else { return };
        let mut start: Option<String> = None;
        let busy = self.worker.is_some();

        xp::group(ui, "Enter the room code from your partner", |ui| {
            ui.horizontal(|ui| {
                let resp = xp::text_box(ui, input, "e.g. MINTY-OTTER-42", 260.0);
                let enabled = !busy && !input.trim().is_empty();
                let go = xp::button(ui, "Join", enabled, true).clicked()
                    || (enabled && resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                if go {
                    // Check the format here, so typos are reported instantly.
                    if collab::code::decode(input).is_ok() || collab::rooms::normalise(input).is_some() {
                        start = Some(input.trim().to_string());
                    } else {
                        *error = Some("That isn't a room code (like MINTY-OTTER-42).".into());
                    }
                }
            });
            ui.label(RichText::new("A long invite code (VTC1-...) works here too.").size(11.0).color(colour::TEXT_DISABLED));
            if let Some(e) = error {
                ui.colored_label(colour::ERROR, e.as_str());
            }
        });
        ui.add_space(8.0);

        if let Some((code, partner)) = reply {
            // Long-code mode: the reply has to be sent back by hand.
            let title = format!("Send this reply code back to {partner}");
            xp::group(ui, &title, |ui| {
                code_preview(ui, code, &mut self.copied_at);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!("Waiting for {partner} to connect..."));
                });
            });
        } else if busy {
            xp::group(ui, "Joining", |ui| {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(match waiting_for {
                        Some(p) => format!("Joined {p}'s room. Connecting..."),
                        None => "Joining the room...".into(),
                    });
                });
            });
        }

        if let Some(text) = start {
            *error = None;
            let worker = Worker::join(self.source.clone().unwrap(), self.name.trim().into(), text, ui.ctx().clone());
            self.worker = Some(worker);
            self.status = "Joining...".into();
        }

        ui.add_space(10.0);
        if xp::button(ui, "Cancel", true, false).clicked() {
            self.reset("Cancelled.");
        }
    }

    fn live_screen(&mut self, ui: &mut egui::Ui) {
        let Screen::Live { partner } = &self.screen else { return };
        let partner = partner.clone();

        xp::group(ui, "Collab", |ui| {
            ui.horizontal(|ui| {
                let (dot, text) = match self.connection {
                    Some(ConnState::Connected) => (colour::OK, format!("Connected to {partner}")),
                    Some(ConnState::Failed) => (colour::ERROR, "Connection failed".to_string()),
                    Some(ConnState::Disconnected) => (colour::WAIT, "Connection lost — retrying...".to_string()),
                    _ => (colour::WAIT, format!("Connecting to {partner}...")),
                };
                xp::status_dot(ui, dot);
                ui.label(RichText::new(text).font(theme::bold(13.0)));
            });
            ui.add_space(6.0);

            if self.connection == Some(ConnState::Failed) {
                ui.label(
                    "No direct route between your PCs. One of your networks (or a firewall) is blocking \
                     direct connections. Check that VTube Collab is allowed through Windows Firewall, \
                     or try another network.",
                );
                return;
            }

            let source = self.publishing.clone().unwrap_or_else(|| format!("{}{partner}", collab::PARTNER_PREFIX));
            ui.label("In OBS, add a Spout2 Capture source and pick:");
            ui.label(RichText::new(format!("    {source}")).font(theme::bold(12.0)));
            ui.add_space(6.0);
            egui::Grid::new("stats").num_columns(2).spacing(vec2(16.0, 4.0)).show(ui, |ui| {
                let fps = |v: Option<f64>| v.map(|f| format!("{f:.0} fps")).unwrap_or_else(|| "—".into());
                ui.label("Sending:");
                ui.label(fps(self.stats.map(|s| s.0)));
                ui.end_row();
                ui.label("Receiving:");
                ui.label(fps(self.stats.map(|s| s.1)));
                ui.end_row();
            });
        });

        ui.add_space(10.0);
        if xp::button(ui, "Leave collab", true, false).clicked() {
            self.reset("You left the collab.");
        }
    }
}

/// "Copy" button that puts `text` on the clipboard and briefly shows "Copied!".
fn copy_button(ui: &mut egui::Ui, text: &str, copied_at: &mut Option<Instant>) {
    if xp::button(ui, "Copy", true, false).clicked() {
        ui.ctx().copy_text(text.to_string());
        *copied_at = Some(Instant::now());
    }
    if copied_at.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
        ui.label(RichText::new("Copied!").color(colour::OK));
        ui.ctx().request_repaint_after(Duration::from_millis(200));
    }
}

/// A shortened long code with a "Copy" button (long codes are ~1000 characters).
fn code_preview(ui: &mut egui::Ui, code: &str, copied_at: &mut Option<Instant>) {
    ui.horizontal(|ui| {
        let short: String = code.chars().take(30).collect();
        ui.label(RichText::new(format!("{short}…")).monospace().background_color(Color32::WHITE));
        copy_button(ui, code, copied_at);
    });
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0] // fully transparent outside our painted window shape
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.handle_worker_messages();

        let window = ui.max_rect();
        xp::window_frame(ui, window);
        xp::title_bar(ui, window, "VTube Collab");

        let status_rect = Rect::from_min_max(
            pos2(window.left() + 3.0, window.bottom() - 3.0 - STATUS_BAR_HEIGHT),
            pos2(window.right() - 3.0, window.bottom() - 3.0),
        );
        let content = Rect::from_min_max(
            pos2(window.left() + 14.0, window.top() + xp::TITLE_BAR_HEIGHT + 12.0),
            pos2(window.right() - 14.0, status_rect.top() - 8.0),
        );
        ui.scope_builder(UiBuilder::new().max_rect(content), |ui| match self.screen {
            Screen::Setup => self.setup_screen(ui),
            Screen::Host { .. } => self.host_screen(ui),
            Screen::Join { .. } => self.join_screen(ui),
            Screen::Live { .. } => self.live_screen(ui),
        });
        xp::status_bar(ui, status_rect, &self.status);
    }
}
