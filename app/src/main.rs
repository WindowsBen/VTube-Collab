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
use eframe::egui::{self, Color32, Rect, UiBuilder, pos2, vec2};

use theme::colour;
use worker::{FromWorker, ToWorker, Worker};

const WINDOW_SIZE: [f32; 2] = [470.0, 400.0];
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

/// Which screen is showing, and what it needs to remember.
enum Screen {
    Setup,
    Host { invite: Option<String>, reply_input: String, error: Option<String> },
    Join { invite_input: String, reply: Option<(String, String)>, error: Option<String> },
    Live { partner: String },
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
        self.connection = None;
        self.publishing = None;
        self.stats = None;
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
                FromWorker::Invite(code) => {
                    if let Screen::Host { invite, .. } = &mut self.screen {
                        *invite = Some(code);
                    }
                    self.status = "Invite code ready. Send it to your partner.".into();
                }
                FromWorker::ReplyRejected(e) => {
                    if let Screen::Host { error, .. } = &mut self.screen {
                        *error = Some(format!("That code didn't work: {e}"));
                    }
                }
                FromWorker::Accepted(partner) => {
                    self.status = format!("Connecting to {partner}...");
                    self.screen = Screen::Live { partner };
                }
                FromWorker::Reply { code, partner } => {
                    if let Screen::Join { reply, .. } = &mut self.screen {
                        *reply = Some((code, partner.clone()));
                    }
                    self.status = format!("Reply code ready. Send it back to {partner}.");
                }
                FromWorker::Ended(error) => {
                    // Show errors on the Setup screen too, so they're hard to miss.
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
                    if let Screen::Join { reply: Some((_, partner)), .. } = &self.screen {
                        self.screen = Screen::Live { partner: partner.clone() };
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
                egui::RichText::new(
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
                let worker = Worker::host(self.source.clone().unwrap(), self.name.trim().into(), ui.ctx().clone());
                self.worker = Some(worker);
                self.setup_error = None;
                self.screen = Screen::Host { invite: None, reply_input: String::new(), error: None };
                self.status = "Starting...".into();
            }
            if xp::button(ui, "Join a collab", ready, false).clicked() {
                self.setup_error = None;
                self.screen = Screen::Join { invite_input: String::new(), reply: None, error: None };
                self.status = "Paste the invite code from your partner.".into();
            }
        });
        if !ready {
            ui.label(egui::RichText::new("Enter your name and pick your avatar to start.").size(11.0).color(colour::TEXT_DISABLED));
        }
        if let Some(e) = &self.setup_error {
            ui.add_space(4.0);
            ui.colored_label(colour::ERROR, e);
        }
    }

    fn host_screen(&mut self, ui: &mut egui::Ui) {
        let Screen::Host { invite, reply_input, error } = &mut self.screen else { return };

        xp::group(ui, "Step 1 — Send this invite code to your partner", |ui| match invite {
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Preparing your invite code...");
                });
            }
            Some(code) => {
                code_preview(ui, code, &mut self.copied_at);
            }
        });
        ui.add_space(8.0);

        let mut connect = false;
        xp::group(ui, "Step 2 — Paste your partner's reply code", |ui| {
            ui.horizontal(|ui| {
                let resp = xp::text_box(ui, reply_input, "VTC1-...", 300.0);
                let enabled = invite.is_some() && !reply_input.trim().is_empty();
                connect = xp::button(ui, "Connect", enabled, true).clicked()
                    || (enabled && resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
            });
            if let Some(e) = error {
                ui.colored_label(colour::ERROR, e.as_str());
            }
        });
        if connect {
            *error = None;
            if let Some(w) = &self.worker {
                w.send(ToWorker::Reply(reply_input.trim().to_string()));
            }
        }

        ui.add_space(10.0);
        if xp::button(ui, "Cancel", true, false).clicked() {
            self.reset("Cancelled.");
        }
    }

    fn join_screen(&mut self, ui: &mut egui::Ui) {
        let Screen::Join { invite_input, reply, error } = &mut self.screen else { return };
        let mut start: Option<String> = None;

        xp::group(ui, "Step 1 — Paste the invite code from your partner", |ui| {
            ui.horizontal(|ui| {
                let enabled = reply.is_none() && self.worker.is_none();
                let resp = xp::text_box(ui, invite_input, "VTC1-...", 300.0);
                let go = xp::button(ui, "Next", enabled && !invite_input.trim().is_empty(), true).clicked()
                    || (enabled && resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                if go {
                    // Check the code here first, so a bad paste is reported instantly.
                    match collab::code::decode(invite_input) {
                        Ok(_) => start = Some(invite_input.trim().to_string()),
                        Err(e) => *error = Some(format!("That code didn't work: {e}")),
                    }
                }
            });
            if let Some(e) = error {
                ui.colored_label(colour::ERROR, e.as_str());
            }
        });
        ui.add_space(8.0);

        match reply {
            Some((code, partner)) => {
                let title = format!("Step 2 — Send this reply code back to {partner}");
                xp::group(ui, &title, |ui| {
                    code_preview(ui, code, &mut self.copied_at);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(format!("Waiting for {partner} to connect..."));
                    });
                });
            }
            None if self.worker.is_some() => {
                xp::group(ui, "Step 2 — Reply code", |ui| {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Preparing your reply code...");
                    });
                });
            }
            None => {}
        }

        if let Some(invite) = start {
            *error = None;
            let worker = Worker::join(self.source.clone().unwrap(), self.name.trim().into(), invite, ui.ctx().clone());
            self.worker = Some(worker);
            self.status = "Starting...".into();
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
                ui.label(egui::RichText::new(text).font(theme::bold(13.0)));
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
            ui.label(egui::RichText::new(format!("    {source}")).font(theme::bold(12.0)));
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

/// A shortened code with a "Copy" button (codes are ~1000 characters long).
fn code_preview(ui: &mut egui::Ui, code: &str, copied_at: &mut Option<Instant>) {
    ui.horizontal(|ui| {
        let short: String = code.chars().take(34).collect();
        ui.label(egui::RichText::new(format!("{short}…")).monospace().background_color(Color32::WHITE));
        if xp::button(ui, "Copy", true, true).clicked() {
            ui.ctx().copy_text(code.to_string());
            *copied_at = Some(Instant::now());
        }
        if copied_at.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
            ui.label(egui::RichText::new("Copied!").color(colour::OK));
            ui.ctx().request_repaint_after(Duration::from_millis(200));
        }
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
