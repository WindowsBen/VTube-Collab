//! VTube Collab — the app window.
//!
//! Screens: Setup → Room (normal) — or the long-code fallback screens.
//! The collab itself runs on a background thread (worker.rs); this file only draws
//! and reacts to clicks.

// Release builds open without a black console window behind the app.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod theme;
mod worker;
mod xp;

use std::time::{Duration, Instant};

use collab::{ConnState, Event, PeerId, RoomEvent, RoomSettings};
use eframe::egui::{self, Color32, Rect, RichText, UiBuilder, pos2, vec2};

use theme::colour;
use worker::{FromWorker, ToWorker, Worker};

const WINDOW_SIZE: [f32; 2] = [500.0, 620.0];
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

/// Where joining is at.
enum JoinPhase {
    /// Typing the code.
    Idle,
    /// Connecting to the room.
    Joining,
    /// In the room's waiting area until the host lets us in.
    Waiting,
    /// Long-code fallback: our reply code, to send back by hand to this partner.
    LongReply { code: String, partner: String },
}

/// Which screen is showing, and what it needs to remember.
enum Screen {
    Setup,
    /// Host: choose room settings before opening the room.
    HostSettings { settings: RoomSettings },
    Join { input: String, phase: JoinPhase, error: Option<String> },
    /// Long-code fallback for hosting (room server unreachable).
    HostLong { why: String, invite: Option<String>, reply_input: String, error: Option<String> },
    /// In a collab. `code` = None for a long-code (two-person) collab, or while the room opens.
    Room { code: Option<String>, host: bool, long_codes: bool },
}

/// Someone else in the collab.
struct Member {
    id: PeerId,
    name: String,
    state: Option<ConnState>,
    source: Option<String>,
    fps: Option<f64>,
    loss_percent: Option<f64>,
    unrepaired: Option<u32>,
    round_trip_ms: Option<f64>,
}

struct App {
    name: String,
    sources: Vec<String>,
    source: Option<String>,
    last_refresh: Instant,
    screen: Screen,
    worker: Option<Worker>,
    // What the collab has told us so far.
    status: String,
    members: Vec<Member>,
    requests: Vec<(PeerId, String)>,
    sent_fps: Option<f64>,
    group_name: Option<String>,
    group_layers: Option<collab::GroupLayers>,
    /// The room's settings, once we're in.
    room_settings: Option<RoomSettings>,
    server_lost: bool,
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
            members: Vec::new(),
            requests: Vec::new(),
            sent_fps: None,
            group_name: None,
            group_layers: None,
            room_settings: None,
            server_lost: false,
            copied_at: None,
            setup_error: None,
        }
    }

    /// Back to the start (stops any collab; dropping the worker cleans up).
    fn reset(&mut self, status: impl Into<String>) {
        self.worker = None;
        self.screen = Screen::Setup;
        self.status = status.into();
        self.clear_collab_info();
    }

    fn clear_collab_info(&mut self) {
        self.members.clear();
        self.requests.clear();
        self.sent_fps = None;
        self.group_name = None;
        self.group_layers = None;
        self.room_settings = None;
        self.server_lost = false;
    }

    fn refresh_sources(&mut self) {
        self.sources = collab::avatar_sources();
        if self.source.as_ref().is_none_or(|s| !self.sources.contains(s)) {
            self.source = self.sources.first().cloned();
        }
        self.last_refresh = Instant::now();
    }

    fn member(&mut self, id: &str) -> Option<&mut Member> {
        self.members.iter_mut().find(|m| m.id == id)
    }

    fn add_member(&mut self, id: &str, name: &str) {
        if self.member(id).is_none() {
            self.members.push(Member {
                id: id.into(),
                name: name.into(),
                state: None,
                source: None,
                fps: None,
                loss_percent: None,
                unrepaired: None,
                round_trip_ms: None,
            });
        }
    }

    // ---------------- messages from the collab ----------------

    fn handle_worker_messages(&mut self) {
        let Some(worker) = &self.worker else { return };
        let messages: Vec<FromWorker> = worker.from_worker.try_iter().collect();
        for msg in messages {
            match msg {
                FromWorker::Event(e) => self.handle_event(e),
                FromWorker::Room(e) => self.handle_room_event(e),
                FromWorker::RoomCode(code) => {
                    if let Screen::Room { code: c, .. } = &mut self.screen {
                        *c = Some(code);
                    }
                    self.status = "Room open. Share the room code.".into();
                }
                FromWorker::RoomUnavailable(why) => {
                    self.screen = Screen::HostLong { why, invite: None, reply_input: String::new(), error: None };
                    self.status = "Room codes unavailable; using long codes.".into();
                }
                FromWorker::Invite(code) => {
                    if let Screen::HostLong { invite, .. } = &mut self.screen {
                        *invite = Some(code);
                    }
                }
                FromWorker::ReplyRejected(e) => {
                    if let Screen::HostLong { error, .. } = &mut self.screen {
                        *error = Some(format!("That code didn't work: {e}"));
                    }
                }
                FromWorker::Accepted(partner) => {
                    self.add_member("partner", &partner);
                    self.screen = Screen::Room { code: None, host: true, long_codes: true };
                    self.status = format!("Connecting to {partner}...");
                }
                FromWorker::Reply { code, partner } => {
                    if let Screen::Join { phase, .. } = &mut self.screen {
                        *phase = JoinPhase::LongReply { code, partner: partner.clone() };
                    }
                    self.status = format!("Reply code ready. Send it back to {partner}.");
                }
                FromWorker::Group(layers) => self.group_layers = Some(layers),
                FromWorker::Ended(error) => {
                    self.worker = None;
                    // Joining failed before getting in (wrong code, declined...): stay on the
                    // Join screen so it can be retried. Anything else: back to Setup.
                    if let (Screen::Join { error: join_error, phase, .. }, Some(e)) = (&mut self.screen, &error) {
                        *join_error = Some(format!("Couldn't join: {e}"));
                        *phase = JoinPhase::Idle;
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
            Event::Connection { peer, state } => {
                // Long-code guest: once connected, move to the Room screen.
                if state == ConnState::Connected {
                    if let Screen::Join { phase: JoinPhase::LongReply { partner, .. }, .. } = &self.screen {
                        let partner = partner.clone();
                        self.add_member(&peer, &partner);
                        self.screen = Screen::Room { code: None, host: false, long_codes: true };
                    }
                }
                let name = self.member(&peer).map(|m| m.name.clone()).unwrap_or_else(|| "a partner".into());
                self.status = match state {
                    ConnState::Connecting => format!("Connecting to {name}..."),
                    ConnState::Connected => format!("Connected to {name}."),
                    ConnState::Failed => format!("Couldn't connect to {name}: no direct route between your PCs."),
                    ConnState::Disconnected => format!("Lost the connection to {name}..."),
                    ConnState::Closed => format!("Connection to {name} closed."),
                };
                if let Some(m) = self.member(&peer) {
                    m.state = Some(state);
                }
            }
            Event::PartnerVideo { peer } => {
                let name = self.member(&peer).map(|m| m.name.clone()).unwrap_or_default();
                self.status = format!("Receiving {name}'s avatar.");
            }
            Event::Publishing { peer, source } => {
                if let Some(m) = self.member(&peer) {
                    m.source = Some(source);
                }
            }
            Event::GroupPublishing(name) => self.group_name = Some(name),
            Event::Stats { sent_fps, received } => {
                self.sent_fps = Some(sent_fps);
                for p in received {
                    if let Some(m) = self.member(&p.id) {
                        m.fps = Some(p.fps);
                        m.loss_percent = p.loss_percent;
                        m.unrepaired = p.unrepaired;
                        m.round_trip_ms = p.round_trip_ms;
                    }
                }
            }
            Event::Warning(text) => self.status = format!("Warning: {text}"),
        }
    }

    fn handle_room_event(&mut self, e: RoomEvent) {
        match e {
            RoomEvent::Waiting => {
                if let Screen::Join { phase, .. } = &mut self.screen {
                    *phase = JoinPhase::Waiting;
                }
                self.status = "Waiting for the host to let you in...".into();
            }
            RoomEvent::Admitted { host, settings } => {
                self.room_settings = Some(settings);
                let code = match &self.screen {
                    Screen::Join { input, .. } => collab::rooms::normalise(input),
                    Screen::Room { code, .. } => code.clone(),
                    _ => None,
                };
                self.screen = Screen::Room { code, host, long_codes: false };
                self.status = if host { "Room open. Share the room code.".into() } else { "You're in!".into() };
            }
            RoomEvent::JoinRequest { id, name } => {
                self.status = format!("{name} wants to join.");
                self.requests.push((id, name));
            }
            RoomEvent::JoinCancelled { id } => self.requests.retain(|(r, _)| *r != id),
            RoomEvent::MemberJoined { id, name } => {
                self.requests.retain(|(r, _)| *r != id);
                self.status = format!("{name} joined.");
                self.add_member(&id, &name);
            }
            RoomEvent::MemberLeft { id, name } => {
                self.members.retain(|m| m.id != id);
                self.status = format!("{name} left.");
            }
            RoomEvent::BecameHost => {
                if let Screen::Room { host, .. } = &mut self.screen {
                    *host = true;
                }
                self.status = "The host left — you're the host now.".into();
            }
            RoomEvent::ServerLost => {
                self.server_lost = true;
                self.status = "Lost the room server. Everyone stays connected, but nobody new can join.".into();
            }
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
                xp::text_box(ui, &mut self.name, "How others will see you", 260.0);
                ui.end_row();

                ui.label("Your avatar:");
                let selected = self.source.clone().unwrap_or_else(|| "(no Spout2 sources found)".into());
                egui::ComboBox::from_id_salt("avatar").selected_text(selected).width(260.0).show_ui(ui, |ui| {
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
                self.setup_error = None;
                self.screen = Screen::HostSettings { settings: RoomSettings::default() };
                self.status = "Choose the room settings.".into();
            }
            if xp::button(ui, "Join a collab", ready, false).clicked() {
                self.setup_error = None;
                self.screen = Screen::Join { input: String::new(), phase: JoinPhase::Idle, error: None };
                self.status = "Enter the room code you were given.".into();
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

    fn host_settings_screen(&mut self, ui: &mut egui::Ui) {
        let Screen::HostSettings { settings } = &mut self.screen else { return };
        let mut open = false;

        xp::group(ui, "Room settings", |ui| {
            egui::Grid::new("room-settings").num_columns(2).spacing(vec2(12.0, 10.0)).show(ui, |ui| {
                ui.label("Max people:");
                ui.add(
                    egui::Slider::new(&mut settings.max_people, collab::MIN_PEOPLE..=collab::MAX_PEOPLE)
                        .suffix(" people"),
                );
                ui.end_row();

                ui.label("Receive buffer:");
                ui.add(
                    egui::Slider::new(&mut settings.buffer_ms, collab::MIN_BUFFER_MS..=collab::MAX_BUFFER_MS)
                        .suffix(" ms"),
                );
                ui.end_row();
            });
            ui.add_space(4.0);
            let hint = |ui: &mut egui::Ui, text: &str| {
                ui.label(RichText::new(text).size(11.0).color(Color32::from_rgb(0x44, 0x44, 0x44)));
            };
            hint(ui, "Max people counts you too. Everyone sends their avatar to everyone, so bigger rooms \
                      need more upload speed (quality is lowered automatically to compensate).");
            hint(ui, "Receive buffer: how long incoming video is held before it's shown. More = smoother \
                      on bad connections (lost data can be resent in time), but avatars appear later. \
                      120 ms suits most people. Everyone in the room uses this setting.");
            ui.add_space(2.0);
            if xp::button(ui, "Reset to defaults", true, false).clicked() {
                *settings = RoomSettings::default();
            }
        });

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            open = xp::button(ui, "Open room", true, true).clicked();
            if xp::button(ui, "Back", true, false).clicked() {
                self.screen = Screen::Setup;
                self.status = "Ready.".into();
            }
        });

        if open {
            let settings = match &self.screen {
                Screen::HostSettings { settings } => *settings,
                _ => return,
            };
            self.clear_collab_info();
            self.worker = Some(Worker::host(self.source.clone().unwrap(), self.name.trim().into(), settings, ui.ctx().clone()));
            self.screen = Screen::Room { code: None, host: true, long_codes: false };
            self.status = "Opening a room...".into();
        }
    }

    fn join_screen(&mut self, ui: &mut egui::Ui) {
        let Screen::Join { input, phase, error } = &mut self.screen else { return };
        let mut start: Option<String> = None;
        let idle = matches!(phase, JoinPhase::Idle);

        xp::group(ui, "Enter the room code", |ui| {
            ui.horizontal(|ui| {
                let resp = xp::text_box(ui, input, "e.g. MINTY-OTTER-42", 260.0);
                let enabled = idle && !input.trim().is_empty();
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

        match phase {
            JoinPhase::Idle => {}
            JoinPhase::Joining => spinner_group(ui, "Joining", "Connecting to the room..."),
            JoinPhase::Waiting => spinner_group(ui, "Joining", "Waiting for the host to let you in..."),
            JoinPhase::LongReply { code, partner } => {
                let title = format!("Send this reply code back to {partner}");
                xp::group(ui, &title, |ui| {
                    code_preview(ui, code, &mut self.copied_at);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(format!("Waiting for {partner} to connect..."));
                    });
                });
            }
        }

        if let Some(text) = start {
            *error = None;
            *phase = JoinPhase::Joining;
            self.clear_collab_info();
            self.worker = Some(Worker::join(self.source.clone().unwrap(), self.name.trim().into(), text, ui.ctx().clone()));
            self.status = "Joining...".into();
        }

        ui.add_space(10.0);
        if xp::button(ui, "Cancel", true, false).clicked() {
            self.reset("Cancelled.");
        }
    }

    fn host_long_screen(&mut self, ui: &mut egui::Ui) {
        let Screen::HostLong { why, invite, reply_input, error } = &mut self.screen else { return };
        let mut send_reply: Option<String> = None;

        xp::group(ui, "Room codes are unavailable right now", |ui| {
            ui.label(RichText::new(format!("({why})")).size(11.0).color(colour::TEXT_DISABLED));
            ui.label("You can still collab with one person using long codes:");
        });
        ui.add_space(8.0);
        xp::group(ui, "Long codes", |ui| {
            match invite {
                Some(code) => {
                    ui.label("1. Send this invite code to your partner:");
                    code_preview(ui, code, &mut self.copied_at);
                }
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Preparing the invite code...");
                    });
                }
            }
            ui.label("2. Paste their reply code:");
            ui.horizontal(|ui| {
                let resp = xp::text_box(ui, reply_input, "VTC1-...", 300.0);
                let enabled = invite.is_some() && !reply_input.trim().is_empty();
                let go = xp::button(ui, "Connect", enabled, true).clicked()
                    || (enabled && resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                if go {
                    send_reply = Some(reply_input.trim().to_string());
                }
            });
            if let Some(e) = error {
                ui.colored_label(colour::ERROR, e.as_str());
            }
        });
        if let Some(reply) = send_reply {
            *error = None;
            if let Some(w) = &self.worker {
                w.send(ToWorker::Reply(reply));
            }
        }

        ui.add_space(10.0);
        if xp::button(ui, "Cancel", true, false).clicked() {
            self.reset("Cancelled.");
        }
    }

    fn room_screen(&mut self, ui: &mut egui::Ui) {
        let Screen::Room { code, host, long_codes } = &self.screen else { return };
        let (code, host, long_codes) = (code.clone(), *host, *long_codes);

        // ---- Room code ----
        if long_codes {
            xp::group(ui, "Collab", |ui| {
                ui.label("Two-person collab (long codes).");
            });
        } else {
            xp::group(ui, "Room code", |ui| match &code {
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Opening a room...");
                    });
                }
                Some(code) => {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(code.as_str()).font(theme::bold(20.0)).color(colour::GROUP_TITLE));
                        ui.add_space(8.0);
                        copy_button(ui, code, &mut self.copied_at);
                    });
                    let hint = if self.server_lost {
                        "The room server can't be reached, so nobody new can join right now."
                    } else if host {
                        "Share this code. You decide who gets in."
                    } else {
                        "Others can join with this code (the host lets them in)."
                    };
                    ui.label(RichText::new(hint).size(11.0).color(colour::TEXT_DISABLED));
                    if let Some(s) = &self.room_settings {
                        let line = format!("Up to {} people · {} ms receive buffer", s.max_people, s.buffer_ms);
                        ui.label(RichText::new(line).size(11.0).color(colour::TEXT_DISABLED));
                    }
                }
            });
        }
        ui.add_space(6.0);

        // ---- Join requests (host) ----
        if host && !self.requests.is_empty() {
            let mut decisions: Vec<ToWorker> = Vec::new();
            xp::group(ui, "Asking to join", |ui| {
                for (id, name) in &self.requests {
                    ui.horizontal(|ui| {
                        xp::status_dot(ui, colour::WAIT);
                        ui.label(RichText::new(name).font(theme::bold(12.0)));
                        ui.label("wants to join");
                        if xp::button(ui, "Accept", true, true).clicked() {
                            decisions.push(ToWorker::Approve(id.clone()));
                        }
                        if xp::button(ui, "Decline", true, false).clicked() {
                            decisions.push(ToWorker::Decline(id.clone()));
                        }
                    });
                }
            });
            for d in decisions {
                let id = match &d {
                    ToWorker::Approve(id) | ToWorker::Decline(id) => id.clone(),
                    ToWorker::Reply(_) => continue,
                };
                self.requests.retain(|(r, _)| *r != id);
                if let Some(w) = &self.worker {
                    w.send(d);
                }
            }
            ui.add_space(6.0);
        }

        // ---- People ----
        let sent = self.sent_fps.map(|f| format!("sending {f:.0} fps")).unwrap_or_default();
        xp::group(ui, &format!("People ({} with you)", self.members.len() + 1), |ui| {
            if self.members.is_empty() {
                ui.label(RichText::new("Nobody else here yet.").color(colour::TEXT_DISABLED));
            }
            for m in &self.members {
                ui.horizontal(|ui| {
                    let (dot, state) = match m.state {
                        Some(ConnState::Connected) => (colour::OK, "connected"),
                        Some(ConnState::Failed) => (colour::ERROR, "couldn't connect"),
                        Some(ConnState::Disconnected) => (colour::WAIT, "connection lost"),
                        Some(ConnState::Closed) => (colour::TEXT_DISABLED, "closed"),
                        _ => (colour::WAIT, "connecting..."),
                    };
                    xp::status_dot(ui, dot);
                    ui.label(RichText::new(&m.name).font(theme::bold(12.0)));
                    ui.label(RichText::new(state).color(colour::TEXT_DISABLED));
                    if let Some(f) = m.fps {
                        ui.label(RichText::new(format!("· {f:.0} fps")).color(colour::TEXT_DISABLED));
                    }
                });
                // Connection quality: loss in red when it's high enough to notice.
                let mut quality = Vec::new();
                if let Some(ms) = m.round_trip_ms {
                    quality.push(format!("{ms:.0} ms round trip"));
                }
                if let Some(loss) = m.loss_percent {
                    quality.push(format!("{loss:.1}% packet loss"));
                }
                // Losses resending couldn't fix in time = visible stutter. If this keeps
                // happening, the host should raise the receive buffer.
                if let Some(u) = m.unrepaired.filter(|u| *u > 0) {
                    quality.push(format!("{u} skipped (try a bigger buffer)"));
                }
                if !quality.is_empty() {
                    let bad = m.unrepaired.is_some_and(|u| u > 2);
                    let colour = if bad { colour::ERROR } else { colour::TEXT_DISABLED };
                    ui.label(RichText::new(format!("      {}", quality.join(" · "))).size(11.0).color(colour));
                }
                let source = m.source.clone().unwrap_or_else(|| collab::group::partner_source(&m.name));
                ui.label(RichText::new(format!("      OBS source: {source}")).size(11.0).color(colour::TEXT_DISABLED));
            }
            if !sent.is_empty() {
                ui.label(RichText::new(format!("You: {sent}")).size(11.0).color(colour::TEXT_DISABLED));
            }
            if self.members.iter().any(|m| m.state == Some(ConnState::Failed)) {
                ui.colored_label(
                    colour::ERROR,
                    "\"Couldn't connect\" = no direct route between your PCs (strict network or firewall). \
                     Check that VTube Collab is allowed through Windows Firewall.",
                );
            }
        });

        ui.add_space(6.0);
        self.layers_panel(ui);

        ui.add_space(10.0);
        let leave = if long_codes { "Leave collab" } else { "Leave room" };
        if xp::button(ui, leave, true, false).clicked() {
            self.reset("You left.");
        }
    }

    /// The group source's layers, photo-editor style: top layer first, a checkbox to
    /// show/hide each one (your own row = "include me"), arrows to change the order.
    /// Changes go straight into the shared list; the group source updates next frame.
    fn layers_panel(&mut self, ui: &mut egui::Ui) {
        let Some(layers) = &self.group_layers else { return };
        let name = self.group_name.clone().unwrap_or_else(|| collab::GROUP_SOURCE.into());
        let title = format!("Group source: {name}");

        xp::group(ui, &title, |ui| {
            let mut list = layers.lock().unwrap();
            if list.is_empty() {
                ui.label("Starting...");
                return;
            }
            let count = list.len();
            let mut move_up: Option<usize> = None;
            let mut move_down: Option<usize> = None;
            for (i, layer) in list.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut layer.visible, "");
                    let text = RichText::new(&layer.label);
                    let text = if layer.visible { text } else { text.color(colour::TEXT_DISABLED) };
                    let label_width = ui.available_width() - 2.0 * 22.0 - 12.0;
                    ui.add_sized([label_width, 20.0], egui::Label::new(text).truncate());
                    if xp::arrow_button(ui, true, i > 0).on_hover_text("Move up (in front)").clicked() {
                        move_up = Some(i);
                    }
                    if xp::arrow_button(ui, false, i + 1 < count).on_hover_text("Move down (behind)").clicked() {
                        move_down = Some(i);
                    }
                });
            }
            if let Some(i) = move_up {
                list.swap(i, i - 1);
            }
            if let Some(i) = move_down {
                list.swap(i, i + 1);
            }
            ui.label(
                RichText::new("Top of the list = in front. Untick to hide (untick yourself to leave you out).")
                    .size(11.0)
                    .color(colour::TEXT_DISABLED),
            );
        });
    }
}

fn spinner_group(ui: &mut egui::Ui, title: &str, text: &str) {
    xp::group(ui, title, |ui| {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(text);
        });
    });
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
        ui.scope_builder(UiBuilder::new().max_rect(content), |ui| {
            // Scrolls if a big room doesn't fit.
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match self.screen {
                Screen::Setup => self.setup_screen(ui),
                Screen::HostSettings { .. } => self.host_settings_screen(ui),
                Screen::Join { .. } => self.join_screen(ui),
                Screen::HostLong { .. } => self.host_long_screen(ui),
                Screen::Room { .. } => self.room_screen(ui),
            });
        });
        xp::status_bar(ui, status_rect, &self.status);
    }
}
