//! VTube Collab — the app window.
//!
//! Screens: Setup → (Room settings →) Room — or the long-code fallback screens.
//! The collab itself runs on a background thread (worker.rs); this file only draws
//! and reacts to clicks. The window resizes itself to fit each screen.

// Release builds open without a black console window behind the app.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod icon;
mod prefs;
mod preview;
mod theme;
mod worker;
mod xp;

use std::sync::Arc;
use std::time::{Duration, Instant};

use collab::{ConnState, Event, PeerId, RoomEvent, RoomSettings};
use eframe::egui::{self, Align, Color32, Layout, Rect, RichText, UiBuilder, pos2, vec2};

use theme::colour;
use worker::{FromWorker, ToWorker, Worker};

const WINDOW_WIDTH: f32 = 470.0;
/// The window grows with its contents up to this height, then scrolls.
const MAX_WINDOW_HEIGHT: f32 = 860.0;
/// Space between the window edge and the contents (XP dialogs use about 7–11 px).
const MARGIN: f32 = 10.0;

fn main() -> eframe::Result {
    collab::log::start("app");
    collab::quiet_gstreamer_log();
    let icon = app_icon();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("VTube Collab")
            .with_inner_size([WINDOW_WIDTH, 300.0])
            .with_resizable(false)
            .with_decorations(false) // we draw our own XP title bar
            .with_transparent(true) // so the rounded top corners can be see-through
            .with_icon(Arc::new(icon)),
        ..Default::default()
    };
    eframe::run_native("VTube Collab", options, Box::new(|cc| Ok(Box::new(App::new(&cc.egui_ctx)))))
}

/// The window icon (title bar, Alt+Tab). 64 px so it stays sharp when Windows scales it.
/// (Explorer, the taskbar and Task Manager use the icon built into the .exe, see build.rs.)
fn app_icon() -> egui::IconData {
    egui::IconData { rgba: icon::rgba(64), width: 64, height: 64 }
}

/// Where joining is at.
pub enum JoinPhase {
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
pub enum Screen {
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
pub struct Member {
    pub id: PeerId,
    pub name: String,
    pub state: Option<ConnState>,
    pub source: Option<String>,
    pub fps: Option<f64>,
    pub loss_percent: Option<f64>,
    pub unrepaired: Option<u32>,
    pub round_trip_ms: Option<f64>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Menu {
    File,
    Help,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Dialog {
    About,
    HowTo,
}

impl Dialog {
    fn size(self) -> egui::Vec2 {
        match self {
            Dialog::About => vec2(360.0, 284.0),
            Dialog::HowTo => vec2(360.0, 288.0),
        }
    }
}

pub struct App {
    pub name: String,
    pub sources: Vec<String>,
    pub source: Option<String>,
    last_refresh: Instant,
    pub screen: Screen,
    worker: Option<Worker>,
    // What the collab has told us so far.
    pub status: String,
    pub members: Vec<Member>,
    pub requests: Vec<(PeerId, String)>,
    pub sent_fps: Option<f64>,
    pub group_name: Option<String>,
    pub group_layers: Option<collab::GroupLayers>,
    /// The group source's on/off switch (shared with the collab; off by default).
    pub group_enabled: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// The room's settings, once we're in.
    pub room_settings: Option<RoomSettings>,
    server_lost: bool,
    /// Host: the person whose Remove button was clicked (waiting for "are you sure?").
    pub confirm_remove: Option<String>,
    /// Host: the receive buffer being chosen in the room (before "Apply").
    pub buffer_edit: Option<u32>,
    /// A newer version, if the update check (in the background, at start) found one.
    pub new_version: Arc<std::sync::Mutex<Option<collab::update::NewVersion>>>,
    copied_at: Option<Instant>,
    setup_error: Option<String>,
    // Window chrome.
    pub open_menu: Option<Menu>,
    /// Open About / How-to windows, with where to first place each (beside the main window).
    pub dialogs: Vec<(Dialog, Option<egui::Pos2>)>,
    icon: Arc<egui::IconData>,
    /// Last window height we asked for (so we only ask when it changes).
    requested_height: f32,
    /// Preview mode (screenshots only): no background refresh of the source list.
    pub preview: bool,
    /// Room settings last used to open a room (remembered for next time).
    last_room_settings: RoomSettings,
    /// The choices as last saved to disk (to save only when something changed).
    saved: prefs::Prefs,
}

impl App {
    fn new(ctx: &egui::Context) -> App {
        theme::apply(ctx);
        let sources = collab::avatar_sources();
        let saved = prefs::load();
        // Last time's avatar if it's running now, otherwise the first one found.
        let source = saved.source.clone().filter(|s| sources.contains(s)).or_else(|| sources.first().cloned());
        let mut app = App {
            name: saved.name.clone(),
            source,
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
            group_enabled: None,
            room_settings: None,
            server_lost: false,
            confirm_remove: None,
            buffer_edit: None,
            new_version: Arc::default(),
            copied_at: None,
            setup_error: None,
            open_menu: None,
            dialogs: Vec::new(),
            icon: Arc::new(app_icon()),
            requested_height: 0.0,
            preview: false,
            last_room_settings: saved.room_settings,
            saved,
        };
        // TEST ONLY: VTC_PREVIEW=room (etc.) shows a screen with sample data, for screenshots.
        if let Ok(which) = std::env::var("VTC_PREVIEW") {
            preview::apply(&mut app, &which);
        } else {
            // Look for a newer version in the background (never slows the start).
            let (found, ctx) = (app.new_version.clone(), ctx.clone());
            std::thread::spawn(move || {
                if let Some(v) = collab::update::check() {
                    *found.lock().unwrap() = Some(v);
                    ctx.request_repaint();
                }
            });
        }
        app
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
        self.group_enabled = None;
        self.room_settings = None;
        self.server_lost = false;
        self.confirm_remove = None;
        self.buffer_edit = None;
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
                FromWorker::Group(layers, enabled) => {
                    // Start the group source the way it was last time.
                    enabled.store(self.saved.group_enabled, std::sync::atomic::Ordering::Relaxed);
                    self.group_layers = Some(layers);
                    self.group_enabled = Some(enabled);
                }
                FromWorker::Ended(error) => {
                    self.worker = None;
                    // Joining failed before getting in (wrong code, declined...): stay on the
                    // Join screen so it can be retried. Anything else: back to Setup.
                    if let (Screen::Join { error: join_error, phase, .. }, Some(e)) = (&mut self.screen, &error) {
                        *join_error = Some(if e.contains(collab::rooms::DAILY_LIMIT) {
                            format!(
                                "Couldn't join: the free room server has used up today's allowance. It works \
                                 again at midnight UTC (in {}). Until then, the host can invite one person \
                                 with a long invite code (it appears when hosting fails), which you can \
                                 paste here.",
                                collab::rooms::until_daily_reset()
                            )
                        } else {
                            format!("Couldn't join: {e}")
                        });
                        *phase = JoinPhase::Idle;
                        self.status = "Couldn't join.".into();
                        return;
                    }
                    self.setup_error = error.as_ref().map(|e| {
                        if e == collab::room::REMOVED {
                            "The host removed you from the room.".to_string()
                        } else {
                            format!("Stopped: {e}")
                        }
                    });
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
            Event::GroupStopped => self.group_name = None,
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
            RoomEvent::SettingsChanged { settings } => {
                self.room_settings = Some(settings);
                self.buffer_edit = None;
                self.status = format!("Receive buffer is now {} ms for everyone.", settings.buffer_ms);
            }
            RoomEvent::JoinRequest { id, name } => {
                self.status = format!("{name} wants to join.");
                // After a reconnect the room sends waiting requests again.
                self.requests.retain(|(r, _)| *r != id);
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
            RoomEvent::ServerReconnecting => {
                self.status = "Room server connection dropped. Reconnecting (everyone stays connected)...".into();
            }
            RoomEvent::ServerBack { host: is_host } => {
                self.server_lost = false;
                if let Screen::Room { host, .. } = &mut self.screen {
                    *host = is_host;
                }
                self.status = "Reconnected to the room server.".into();
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
        if !self.preview && self.last_refresh.elapsed() > Duration::from_secs(2) {
            self.refresh_sources();
        }
        ui.ctx().request_repaint_after(Duration::from_secs(2));

        xp::group(ui, "Your setup", |ui| {
            egui::Grid::new("setup").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
                ui.label("Your name:");
                xp::text_box(ui, &mut self.name, "How others will see you", 270.0);
                ui.end_row();

                ui.label("Your avatar:");
                let selected = self.source.clone().unwrap_or_else(|| "(no Spout2 sources found)".into());
                xp::dropdown(ui, "avatar", &selected, 270.0, |ui| {
                    for s in &self.sources {
                        ui.selectable_value(&mut self.source, Some(s.clone()), s);
                    }
                });
                ui.end_row();
            });
            ui.add_space(2.0);
            xp::hint(
                ui,
                "Your avatar is your VTubing app's Spout2 output. If the list is empty, \
                 turn on Spout2 output in your VTubing app.",
            );
        });

        if let Some(e) = &self.setup_error {
            ui.add_space(4.0);
            ui.colored_label(colour::ERROR, e);
        }
        self.update_notice(ui);
        let ready = !self.name.trim().is_empty() && self.source.is_some();
        if !ready {
            xp::hint(ui, "Enter your name and pick your avatar to start.");
        }

        ui.add_space(4.0);
        button_row(ui, |ui| {
            if xp::button(ui, "Join a collab...", ready, false).clicked() {
                self.setup_error = None;
                self.screen = Screen::Join { input: String::new(), phase: JoinPhase::Idle, error: None };
                self.status = "Enter the room code you were given.".into();
            }
            if xp::button(ui, "Host a collab...", ready, true).clicked() {
                self.setup_error = None;
                self.screen = Screen::HostSettings { settings: self.last_room_settings };
                self.status = "Choose the room settings.".into();
            }
        });
    }

    /// "Version X is available" with a button to its download page (if there is one).
    fn update_notice(&self, ui: &mut egui::Ui) {
        let Some(v) = self.new_version.lock().unwrap().clone() else { return };
        xp::group(ui, "Update available", |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("VTube Collab {} is out (you have {}).", v.version, env!("CARGO_PKG_VERSION")));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if xp::button(ui, "Download...", true, false).clicked() {
                        open_web_page(&v.page);
                    }
                });
            });
        });
    }

    fn host_settings_screen(&mut self, ui: &mut egui::Ui) {
        let Screen::HostSettings { settings } = &mut self.screen else { return };
        let mut open = false;
        let mut back = false;

        xp::group(ui, "Room settings", |ui| {
            egui::Grid::new("room-settings").num_columns(2).spacing(vec2(12.0, 6.0)).show(ui, |ui| {
                ui.label("Max people:");
                xp::slider(ui, &mut settings.max_people, collab::MIN_PEOPLE..=collab::MAX_PEOPLE, 220.0, " people");
                ui.end_row();

                ui.label("Receive buffer:");
                xp::slider(ui, &mut settings.buffer_ms, collab::MIN_BUFFER_MS..=collab::MAX_BUFFER_MS, 220.0, " ms");
                ui.end_row();
            });
            ui.add_space(4.0);
            let (w, h) = collab::transmit_size_for(settings.max_people);
            ui.label(format!("Video size per person: {w}×{h}"));
            xp::hint(
                ui,
                "Max people counts you too. Everyone sends their avatar to everyone, so bigger rooms \
                 use smaller video (1080p up to 4 people, 720p up to 8, 540p above) to keep \
                 upload and graphics card load manageable.",
            );
            xp::hint(
                ui,
                "Receive buffer: how long incoming video is held before it's shown. More = smoother \
                 on bad connections (lost data can be resent in time), but avatars appear later. \
                 120 ms suits most people. Everyone in the room uses this setting.",
            );
            ui.add_space(2.0);
            if xp::button(ui, "Defaults", true, false).clicked() {
                *settings = RoomSettings::default();
            }
        });

        ui.add_space(4.0);
        button_row(ui, |ui| {
            back = xp::button(ui, "< Back", true, false).clicked();
            open = xp::button(ui, "Open room", true, true).clicked();
        });

        if back {
            self.screen = Screen::Setup;
            self.status = "Ready.".into();
        } else if open {
            let Screen::HostSettings { settings } = &self.screen else { return };
            let settings = *settings;
            collab::log::info(&format!("Hosting as {:?} with avatar {:?}, {settings:?}", self.name.trim(), self.source));
            self.last_room_settings = settings;
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

        xp::group(ui, "Join a collab", |ui| {
            ui.label("Room code:");
            let resp = xp::text_box(ui, input, "e.g. MINTY-OTTER-42", ui.available_width());
            xp::hint(ui, "Type the code the host gave you. A long invite code (VTC1-...) works here too.");
            if let Some(e) = error {
                ui.colored_label(colour::ERROR, e.as_str());
            }
            let enter = idle && resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if enter && !input.trim().is_empty() {
                start = Some(input.trim().to_string());
            }
        });

        match phase {
            JoinPhase::Idle => {}
            JoinPhase::Joining => spinner_line(ui, "Connecting to the room..."),
            JoinPhase::Waiting => spinner_line(ui, "Waiting for the host to let you in..."),
            JoinPhase::LongReply { code, partner } => {
                let title = format!("Send this reply code back to {partner}");
                xp::group(ui, &title, |ui| {
                    code_preview(ui, code, &mut self.copied_at);
                    spinner_line(ui, &format!("Waiting for {partner} to connect..."));
                });
            }
        }

        let mut cancel = false;
        ui.add_space(4.0);
        button_row(ui, |ui| {
            cancel = xp::button(ui, "Cancel", true, false).clicked();
            if xp::button(ui, "Join", idle && !input.trim().is_empty(), true).clicked() {
                start = Some(input.trim().to_string());
            }
        });

        if let Some(text) = start {
            // Check the format here, so typos are reported instantly.
            if collab::code::decode(&text).is_ok() || collab::rooms::normalise(&text).is_some() {
                let what = collab::rooms::normalise(&text).unwrap_or_else(|| "a long invite code".into());
                collab::log::info(&format!("Joining {what} as {:?} with avatar {:?}", self.name.trim(), self.source));
                *error = None;
                *phase = JoinPhase::Joining;
                self.clear_collab_info();
                self.worker = Some(Worker::join(self.source.clone().unwrap(), self.name.trim().into(), text, ui.ctx().clone()));
                self.status = "Joining...".into();
            } else {
                *error = Some("That isn't a room code (like MINTY-OTTER-42).".into());
            }
        }
        if cancel {
            self.reset("Cancelled.");
        }
    }

    fn host_long_screen(&mut self, ui: &mut egui::Ui) {
        let Screen::HostLong { why, invite, reply_input, error } = &mut self.screen else { return };
        let mut send_reply: Option<String> = None;

        xp::group(ui, "Room codes are unavailable right now", |ui| {
            if why.contains(collab::rooms::DAILY_LIMIT) {
                ui.label(format!(
                    "The room server runs on a free plan with a daily allowance, and today's is used up. \
                     It works again at midnight UTC (in {}).",
                    collab::rooms::until_daily_reset()
                ));
                xp::hint(ui, "Collabs that are already running are not affected.");
            } else if why.contains(collab::rooms::BUSY) {
                ui.label("Too many rooms were opened from your internet connection in the last minute. \
                          Wait a minute, then click Cancel and host again.");
            } else {
                ui.label("Couldn't open a room. Check that you're online. If other websites work, a firewall \
                          or antivirus may be blocking VTube Collab, or the room server is having trouble: \
                          try again in a few minutes.");
                xp::hint(ui, &format!("({why})"));
            }
            ui.label("Meanwhile you can collab with one person using long codes: send them the invite \
                      code below, they paste it into \"Join a collab\" and send you back their reply code.");
        });
        xp::group(ui, "Long codes", |ui| {
            ui.label("1. Send this invite code to your partner:");
            match invite {
                Some(code) => code_preview(ui, code, &mut self.copied_at),
                None => spinner_line(ui, "Preparing the invite code..."),
            }
            ui.label("2. Paste their reply code:");
            let resp = xp::text_box(ui, reply_input, "VTC1-...", ui.available_width());
            let enabled = invite.is_some() && !reply_input.trim().is_empty();
            if enabled && resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                send_reply = Some(reply_input.trim().to_string());
            }
            if let Some(e) = error {
                ui.colored_label(colour::ERROR, e.as_str());
            }
        });

        let mut cancel = false;
        ui.add_space(4.0);
        button_row(ui, |ui| {
            cancel = xp::button(ui, "Cancel", true, false).clicked();
            let enabled = invite.is_some() && !reply_input.trim().is_empty();
            if xp::button(ui, "Connect", enabled, true).clicked() {
                send_reply = Some(reply_input.trim().to_string());
            }
        });
        if let Some(reply) = send_reply {
            *error = None;
            if let Some(w) = &self.worker {
                w.send(ToWorker::Reply(reply));
            }
        }
        if cancel {
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
                None => spinner_line(ui, "Opening a room..."),
                Some(code) => {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(code.as_str()).font(theme::bold(18.0)).color(colour::GROUP_TITLE));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            copy_button(ui, code, &mut self.copied_at);
                        });
                    });
                    let hint = if self.server_lost {
                        "The room server can't be reached, so nobody new can join right now.".to_string()
                    } else if host {
                        "Share this code. You decide who gets in.".to_string()
                    } else {
                        "Others can join with this code (the host lets them in).".to_string()
                    };
                    xp::hint(ui, &hint);
                    if let Some(s) = self.room_settings {
                        let (w, h) = collab::transmit_size_for(s.max_people);
                        xp::hint(
                            ui,
                            &format!("Up to {} people · {w}×{h} video · {} ms receive buffer", s.max_people, s.buffer_ms),
                        );
                        if host {
                            self.buffer_control(ui, s.buffer_ms);
                        }
                    }
                }
            });
        }

        // ---- Join requests (host) ----
        if host && !self.requests.is_empty() {
            let mut decisions: Vec<ToWorker> = Vec::new();
            xp::group(ui, "Asking to join", |ui| {
                for (id, name) in &self.requests {
                    ui.horizontal(|ui| {
                        xp::status_dot(ui, colour::WAIT);
                        ui.label(RichText::new(name).font(theme::bold(12.0)));
                        ui.label("wants to join");
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if xp::button(ui, "Decline", true, false).clicked() {
                                decisions.push(ToWorker::Decline(id.clone()));
                            }
                            if xp::button(ui, "Accept", true, true).clicked() {
                                decisions.push(ToWorker::Approve(id.clone()));
                            }
                        });
                    });
                }
            });
            for d in decisions {
                let id = match &d {
                    ToWorker::Approve(id) | ToWorker::Decline(id) => id.clone(),
                    ToWorker::Remove(_) | ToWorker::SetBuffer(_) | ToWorker::Reply(_) => continue,
                };
                self.requests.retain(|(r, _)| *r != id);
                if let Some(w) = &self.worker {
                    w.send(d);
                }
            }
        }

        // ---- People ----
        // The host can remove people (rooms only). Asks "are you sure?" in the row first.
        let can_remove = host && !long_codes;
        let mut remove_clicked: Option<String> = None;
        let mut remove_confirmed: Option<String> = None;
        let mut remove_cancelled = false;
        xp::group(ui, &format!("People ({} with you)", self.members.len() + 1), |ui| {
            if self.members.is_empty() {
                xp::hint(ui, "Nobody else here yet.");
            }
            for m in &self.members {
                if can_remove && self.confirm_remove.as_deref() == Some(m.id.as_str()) {
                    ui.horizontal(|ui| {
                        xp::status_dot(ui, colour::ERROR);
                        ui.label(RichText::new(format!("Remove {} from the room?", m.name)).font(theme::bold(12.0)));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if xp::button(ui, "Cancel", true, false).clicked() {
                                remove_cancelled = true;
                            }
                            if xp::button(ui, "Remove", true, true).clicked() {
                                remove_confirmed = Some(m.id.clone());
                            }
                        });
                    });
                    ui.horizontal(|ui| {
                        ui.add_space(18.0);
                        xp::hint(ui, "They can ask to join again; you decide whether to let them back in.");
                    });
                    continue;
                }
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
                    ui.label(RichText::new(state).color(colour::TEXT_HINT));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if can_remove && xp::button(ui, "Remove", true, false).clicked() {
                            remove_clicked = Some(m.id.clone());
                        }
                        if let Some(f) = m.fps {
                            ui.label(format!("{f:.0} fps"));
                        }
                    });
                });
                let source = m.source.clone().unwrap_or_else(|| collab::group::partner_source(&m.name));
                let mut details = vec![format!("OBS source: {source}")];
                if let Some(ms) = m.round_trip_ms {
                    details.push(format!("{ms:.0} ms"));
                }
                if let Some(loss) = m.loss_percent {
                    details.push(format!("{loss:.1}% loss"));
                }
                ui.horizontal(|ui| {
                    ui.add_space(18.0);
                    xp::hint(ui, &details.join(" · "));
                });
                // Losses resending couldn't fix in time = visible stutter.
                if let Some(u) = m.unrepaired.filter(|u| *u > 0) {
                    ui.horizontal(|ui| {
                        ui.add_space(18.0);
                        let text = format!("{u} frames skipped — the host may want a bigger receive buffer");
                        let colour = if u > 2 { colour::ERROR } else { colour::TEXT_HINT };
                        ui.label(RichText::new(text).size(11.0).color(colour));
                    });
                }
            }
            if self.members.iter().any(|m| m.state == Some(ConnState::Failed)) {
                ui.colored_label(
                    colour::ERROR,
                    "\"Couldn't connect\" = no direct route between your PCs (strict network or firewall). \
                     Check that VTube Collab is allowed through Windows Firewall.",
                );
            }
        });

        if let Some(id) = remove_clicked {
            self.confirm_remove = Some(id);
        }
        if remove_cancelled {
            self.confirm_remove = None;
        }
        if let Some(id) = remove_confirmed {
            self.confirm_remove = None;
            if let Some(m) = self.members.iter().find(|m| m.id == id) {
                self.status = format!("Removed {}.", m.name);
            }
            if let Some(w) = &self.worker {
                w.send(ToWorker::Remove(id));
            }
        }

        self.layers_panel(ui);

        let mut leave = false;
        ui.add_space(4.0);
        button_row(ui, |ui| {
            leave = xp::button(ui, if long_codes { "Leave collab" } else { "Leave room" }, true, false).clicked();
        });
        if leave {
            self.reset("You left.");
        }
    }

    /// Host: change the receive buffer for everyone while in the room. The slider only
    /// picks a value; "Apply" sends it (so dragging doesn't send dozens of changes).
    fn buffer_control(&mut self, ui: &mut egui::Ui, current: u32) {
        let mut value = self.buffer_edit.unwrap_or(current);
        let mut apply = false;
        ui.horizontal(|ui| {
            ui.label("Receive buffer:");
            xp::slider(ui, &mut value, collab::MIN_BUFFER_MS..=collab::MAX_BUFFER_MS, 170.0, " ms");
            apply = xp::button(ui, "Apply", value != current && !self.server_lost, false).clicked();
        });
        self.buffer_edit = (value != current).then_some(value);
        if apply {
            if let Some(w) = &self.worker {
                w.send(ToWorker::SetBuffer(value));
            }
            self.status = format!("Changing the receive buffer to {value} ms...");
        }
        if value != current {
            xp::hint(ui, "Applies to everyone in the room. More = smoother on bad connections, but avatars appear later.");
        }
    }

    /// The group source: an on/off switch, and (when on) its layers, photo-editor
    /// style — top layer first, a checkbox to show/hide each one (your own row =
    /// "include me"), arrows to change the order. Changes go straight into the shared
    /// list; the group source updates next frame.
    fn layers_panel(&mut self, ui: &mut egui::Ui) {
        let (Some(layers), Some(enabled)) = (&self.group_layers, &self.group_enabled) else { return };
        let name = self.group_name.clone().unwrap_or_else(|| collab::GROUP_SOURCE.into());

        xp::group(ui, "Group source", |ui| {
            use std::sync::atomic::Ordering;
            let mut on = enabled.load(Ordering::Relaxed);
            if xp::checkbox(ui, &mut on, "Publish a group source (everyone in one OBS source)").changed() {
                enabled.store(on, Ordering::Relaxed);
            }
            if !on {
                xp::hint(
                    ui,
                    "Off: only the individual \"Collab - <name>\" sources are published, \
                     which saves resources. Turn on to get everyone in one source.",
                );
                return;
            }
            xp::hint(ui, &format!("In OBS, add a Spout2 Capture source and pick \"{name}\"."));
            ui.add_space(2.0);

            let mut list = layers.lock().unwrap();
            if list.is_empty() {
                xp::hint(ui, "Starting...");
                return;
            }
            let count = list.len();
            let (mut move_up, mut move_down) = (None, None);
            for (i, layer) in list.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    xp::checkbox(ui, &mut layer.visible, "");
                    let text = RichText::new(&layer.label);
                    let text = if layer.visible { text } else { text.color(colour::TEXT_DISABLED) };
                    ui.label(text);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if xp::arrow_button(ui, false, i + 1 < count).on_hover_text("Move down (behind)").clicked() {
                            move_down = Some(i);
                        }
                        if xp::arrow_button(ui, true, i > 0).on_hover_text("Move up (in front)").clicked() {
                            move_up = Some(i);
                        }
                    });
                });
            }
            if let Some(i) = move_up {
                list.swap(i, i - 1);
            }
            if let Some(i) = move_down {
                list.swap(i, i + 1);
            }
            xp::hint(ui, "Top of the list = in front. Untick to hide (untick yourself to leave you out).");
        });
    }

    // ---------------- menus & dialogs ----------------

    fn menu_bar(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let mut opened_at: Option<(Menu, Rect)> = None;
        ui.scope_builder(UiBuilder::new().max_rect(rect.shrink2(vec2(2.0, 1.0))), |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for (menu, label) in [(Menu::File, "File"), (Menu::Help, "Help")] {
                    let resp = xp::menu_title(ui, label, self.open_menu == Some(menu));
                    // Click opens/closes; while one menu is open, hovering another switches to it.
                    if resp.clicked() {
                        self.open_menu = if self.open_menu == Some(menu) { None } else { Some(menu) };
                    } else if self.open_menu.is_some() && resp.hovered() {
                        self.open_menu = Some(menu);
                    }
                    if self.open_menu == Some(menu) {
                        opened_at = Some((menu, resp.rect));
                    }
                }
            });
        });

        let Some((menu, title_rect)) = opened_at else { return };
        let popup = egui::Area::new(egui::Id::unique("xp-menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(title_rect.left_bottom() + vec2(0.0, 1.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::NONE
                    .fill(Color32::WHITE)
                    .stroke(egui::Stroke::new(1.0, colour::MENU_BORDER))
                    .inner_margin(2)
                    .shadow(egui::Shadow { offset: [2, 2], blur: 2, spread: 0, color: Color32::from_black_alpha(60) })
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        match menu {
                            Menu::File => {
                                if xp::menu_item(ui, "Open log folder").clicked() {
                                    // Explorer opens the folder; testers zip the newest log and send it.
                                    let _ = std::process::Command::new("explorer").arg(collab::log::folder()).spawn();
                                    self.open_menu = None;
                                }
                                xp::menu_separator(ui);
                                if xp::menu_item(ui, "Exit").clicked() {
                                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                                }
                            }
                            Menu::Help => {
                                if xp::menu_item(ui, "How to use...").clicked() {
                                    self.open_dialog(ui.ctx(), Dialog::HowTo);
                                    self.open_menu = None;
                                }
                                if xp::menu_item(ui, "Leave a tip...").clicked() {
                                    open_web_page(collab::update::DONATE);
                                    self.open_menu = None;
                                }
                                xp::menu_separator(ui);
                                if xp::menu_item(ui, "About VTube Collab").clicked() {
                                    self.open_dialog(ui.ctx(), Dialog::About);
                                    self.open_menu = None;
                                }
                            }
                        }
                    });
            });

        // Clicking anywhere outside the menu (and its title) closes it.
        let clicked_outside = ui.ctx().input(|i| {
            i.pointer.any_pressed()
                && i.pointer.interact_pos().is_some_and(|p| !popup.response.rect.contains(p) && !rect.contains(p))
        });
        if clicked_outside || ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
            self.open_menu = None;
        }
    }

    /// Open the About / How-to window (it's placed beside the main window when first drawn).
    fn open_dialog(&mut self, _ctx: &egui::Context, dialog: Dialog) {
        if !self.dialogs.iter().any(|(d, _)| *d == dialog) {
            self.dialogs.push((dialog, None));
        }
    }

    /// Where to first place a window of `size`: to the right of the main window, or to
    /// its left if that would go off-screen. `index` staggers several windows.
    fn beside_main_window(ctx: &egui::Context, size: egui::Vec2, index: usize) -> Option<egui::Pos2> {
        ctx.input(|i| {
            let main = i.viewport().outer_rect?;
            let screen_w = i.viewport().monitor_size.map(|s| s.x).unwrap_or(f32::INFINITY);
            let y = main.top() + 30.0 * index as f32;
            Some(if main.right() + 8.0 + size.x <= screen_w {
                pos2(main.right() + 8.0, y)
            } else {
                pos2((main.left() - 8.0 - size.x).max(0.0), y)
            })
        })
    }

    /// The About / How-to windows: real separate windows with their own XP title bar.
    /// They can be moved anywhere (even outside the main window), and the main window
    /// stays fully usable meanwhile — e.g. read the steps while doing them.
    fn dialog_windows(&mut self, ctx: &egui::Context) {
        // Decide where newly opened windows go (once; after that they stay where dragged).
        for (index, (dialog, pos)) in self.dialogs.iter_mut().enumerate() {
            if pos.is_none() {
                *pos = Self::beside_main_window(ctx, dialog.size(), index);
            }
        }
        let mut closed: Vec<Dialog> = Vec::new();
        for &(dialog, pos) in &self.dialogs {
            let title = match dialog {
                Dialog::About => "About VTube Collab",
                Dialog::HowTo => "How to use VTube Collab",
            };
            let mut builder = egui::ViewportBuilder::default()
                .with_title(title)
                .with_inner_size(dialog.size())
                .with_resizable(false)
                .with_decorations(false) // drawn by us, XP-style
                .with_transparent(true)
                .with_icon(self.icon.clone());
            if let Some(pos) = pos {
                // Only used when the window is first created; afterwards it stays where it's dragged.
                builder = builder.with_position(pos);
            }
            let close = ctx.show_viewport_immediate(egui::ViewportId::from_hash_of(title), builder, |ui, _class| {
                let mut ok = false;
                let ((), x) = xp::tool_window(ui, title, |ui| dialog_contents(ui, dialog, &mut ok));
                let keys = ui.input(|i| i.key_pressed(egui::Key::Escape) || i.viewport().close_requested());
                ok || x || keys
            });
            if close {
                closed.push(dialog);
            }
        }
        self.dialogs.retain(|(d, _)| !closed.contains(d));
    }

    /// Save the remembered choices if any of them changed (not in preview mode).
    fn save_prefs_if_changed(&mut self) {
        if self.preview {
            return;
        }
        let now = prefs::Prefs {
            name: self.name.trim().to_string(),
            source: self.source.clone(),
            room_settings: self.last_room_settings,
            group_enabled: match &self.group_enabled {
                Some(flag) => flag.load(std::sync::atomic::Ordering::Relaxed),
                None => self.saved.group_enabled, // not in a room: keep last time's choice
            },
        };
        if now != self.saved {
            prefs::save(&now);
            self.saved = now;
        }
    }

    fn status_right(&self) -> Option<String> {
        match &self.screen {
            Screen::Room { .. } => {
                let people = format!("{} people", self.members.len() + 1);
                Some(match self.sent_fps {
                    Some(f) => format!("{people} · {f:.0} fps"),
                    None => people,
                })
            }
            _ => None,
        }
    }
}

/// The text inside the About / How-to windows. `ok` is set when OK is clicked.
fn dialog_contents(ui: &mut egui::Ui, dialog: Dialog, ok: &mut bool) {
    match dialog {
        Dialog::About => {
            ui.horizontal(|ui| {
                let (icon, _) = ui.allocate_exact_size(vec2(32.0, 32.0), egui::Sense::hover());
                xp::paint_app_icon(ui.painter(), icon);
                ui.vertical(|ui| {
                    xp::bold_label(ui, "VTube Collab", 13.0);
                    ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                });
            });
            ui.add_space(6.0);
            ui.label("Collab with VTubers using any VTubing app: avatars travel directly between your PCs, with transparency, into OBS.");
            xp::hint(ui, "Open source (MIT licence). Uses GStreamer (LGPL) for video. Licences are in the 'licenses' folder.");
            ui.add_space(2.0);
            xp::hint(ui, "VTube Collab is free and stays free. If you'd like to say thanks, tips are welcome.");
            ui.horizontal(|ui| {
                if xp::button(ui, "Website...", true, false).clicked() {
                    open_web_page(collab::update::WEBSITE);
                }
                if xp::button(ui, "Leave a tip...", true, false).clicked() {
                    open_web_page(collab::update::DONATE);
                }
            });
        }
        Dialog::HowTo => {
            for step in [
                "1. Turn on Spout2 output in your VTubing app.",
                "2. Enter your name and pick your avatar.",
                "3. Host: click \"Host a collab\", choose settings, and share the room code. \
                 Accept people as they ask to join.",
                "4. Join: click \"Join a collab\" and type the room code you were given.",
                "5. In OBS, add a Spout2 Capture source for each person (\"Collab - <name>\"), \
                 or one for everyone together (\"Collab - Group\").",
                "6. Allow VTube Collab through Windows Firewall when asked.",
            ] {
                ui.label(step);
            }
        }
    }
    ui.with_layout(Layout::bottom_up(Align::Max), |ui| {
        if xp::button(ui, "OK", true, true).clicked() {
            *ok = true;
        }
    });
}

/// Open a web page in the default browser.
fn open_web_page(url: &str) {
    if url.starts_with("https://") {
        let _ = std::process::Command::new("explorer").arg(url).spawn();
    }
}

/// A row of buttons aligned to the right, XP-dialog style. Add them in reading order
/// (e.g. Cancel, then OK); the last one ends up at the far right.
fn button_row(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        let width = ui.available_width();
        // Measure the buttons first (invisibly), then place them flush right.
        let id = ui.scope_id().with("button-row-width");
        let used: f32 = ui.data(|d| d.get_temp(id)).unwrap_or(0.0);
        ui.add_space((width - used).max(0.0));
        let start = ui.cursor().left();
        ui.spacing_mut().item_spacing.x = 6.0;
        add(ui);
        let new_used = ui.min_rect().right() - start;
        if (new_used - used).abs() > 0.5 {
            ui.data_mut(|d| d.insert_temp(id, new_used));
            ui.ctx().request_repaint();
        }
    });
}

fn spinner_line(ui: &mut egui::Ui, text: &str) {
    ui.horizontal(|ui| {
        ui.spinner();
        ui.label(text);
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
        let body = xp::window_frame(ui.painter(), window);
        xp::title_bar(ui, window, "VTube Collab");

        // Menu bar, with a thin line under it.
        let menu_rect = Rect::from_min_size(body.min, vec2(body.width(), xp::MENU_BAR_HEIGHT));
        ui.painter().line_segment(
            [menu_rect.left_bottom(), menu_rect.right_bottom()],
            egui::Stroke::new(1.0, colour::GROUP_BORDER),
        );

        let status_rect = Rect::from_min_max(pos2(body.left(), body.bottom() - xp::STATUS_BAR_HEIGHT), body.max);
        let content = Rect::from_min_max(
            pos2(body.left() + MARGIN, menu_rect.bottom() + MARGIN),
            pos2(body.right() - MARGIN, status_rect.top() - MARGIN),
        );

        // Contents; they scroll only if the window has reached its maximum height.
        let max_content = MAX_WINDOW_HEIGHT - (window.height() - content.height());
        let output = ui
            .scope_builder(UiBuilder::new().max_rect(content), |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                egui::ScrollArea::vertical().max_height(max_content).auto_shrink([false, true]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 8.0;
                    match self.screen {
                        Screen::Setup => self.setup_screen(ui),
                        Screen::HostSettings { .. } => self.host_settings_screen(ui),
                        Screen::Join { .. } => self.join_screen(ui),
                        Screen::HostLong { .. } => self.host_long_screen(ui),
                        Screen::Room { .. } => self.room_screen(ui),
                    }
                })
            })
            .inner;

        let right = self.status_right();
        xp::status_bar(ui, status_rect, &self.status, right.as_deref());
        self.menu_bar(ui, menu_rect);
        self.dialog_windows(ui.ctx());
        self.save_prefs_if_changed();

        // Fit the window to its contents (Windows does this for XP dialogs, too).
        let chrome = window.height() - content.height();
        let wanted = (output.content_size.y + chrome).clamp(200.0, MAX_WINDOW_HEIGHT).round();
        if (wanted - window.height()).abs() > 0.5 && (wanted - self.requested_height).abs() > 0.5 {
            self.requested_height = wanted;
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::InnerSize(vec2(WINDOW_WIDTH, wanted)));
        }
    }
}
