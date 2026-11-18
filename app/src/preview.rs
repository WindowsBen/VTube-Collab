//! TEST ONLY — preview mode for screenshots: `VTC_PREVIEW=<screen>` opens a screen
//! filled with sample people and stats, without connecting to anything.
//!
//!   setup, settings, join, waiting, room, guest, long, limit, update, about, howto, menu,
//!   error, remove, buffer
//!   — add "+about" or "+howto" to open that dialog on top, e.g. "setup+howto".

use std::sync::{Arc, Mutex};

use collab::{ConnState, GroupLayer, RoomSettings};

use crate::{App, Dialog, JoinPhase, Member, Screen};

pub fn apply(app: &mut App, which: &str) {
    // "screen+about" / "screen+howto" also opens that dialog on top of the screen.
    let (which, dialog) = which.split_once('+').unwrap_or((which, ""));
    apply_screen(app, which);
    match dialog {
        "about" => app.dialogs.push((Dialog::About, None)),
        "howto" => app.dialogs.push((Dialog::HowTo, None)),
        _ => {}
    }
}

fn apply_screen(app: &mut App, which: &str) {
    app.preview = true;
    app.name = "Ben".into();
    app.sources = vec!["Avatar Live".into(), "Warudo Camera 1".into()];
    app.source = Some("Avatar Live".into());

    match which {
        "settings" => {
            app.screen = Screen::HostSettings { settings: RoomSettings::default() };
            app.status = "Choose the room settings.".into();
        }
        "join" => {
            app.screen = Screen::Join { input: "minty otter 42".into(), phase: JoinPhase::Idle, error: None };
            app.status = "Enter the room code you were given.".into();
        }
        "waiting" => {
            app.screen = Screen::Join { input: "MINTY-OTTER-42".into(), phase: JoinPhase::Waiting, error: None };
            app.status = "Waiting for the host to let you in...".into();
        }
        "long" => {
            app.screen = Screen::HostLong {
                why: "the room server can't be reached".into(),
                invite: Some("VTC1-lZTZbtpAFIZfZcRtAszi2Y7ERcBYTUUTsirrhRmPEzdgu7azqeq7".into()),
                reply_input: String::new(),
                error: None,
            };
            app.status = "Room codes unavailable; using long codes.".into();
        }
        "limit" => {
            app.screen = Screen::HostLong {
                why: collab::rooms::DAILY_LIMIT.into(),
                invite: Some("VTC1-lZTZbtpAFIZfZcRtAszi2Y7ERcBYTUUTsirrhRmPEzdgu7azqeq7".into()),
                reply_input: String::new(),
                error: None,
            };
            app.status = "Room codes unavailable; using long codes.".into();
        }
        "update" => {
            *app.new_version.lock().unwrap() = Some(collab::update::NewVersion {
                version: "0.4.0-beta".into(),
                page: collab::update::WEBSITE.into(),
            });
        }
        "menu" => app.open_menu = Some(crate::Menu::Help),
        "error" => {
            // The avatar is on another graphics card than any we can use.
            let e = "Stopped: Couldn't open your avatar: its image can't be read on any graphics card. \
                     If this PC has two graphics cards, set your VTubing app, OBS and VTube Collab to \
                     \"High performance\" in Windows Settings > System > Display > Graphics, then \
                     restart them.";
            app.setup_error = Some(e.into());
            app.status = e.into();
        }
        "room" | "guest" | "about" | "howto" | "remove" | "buffer" => {
            let host = which != "guest";
            app.screen = Screen::Room { code: Some("MINTY-OTTER-42".into()), host, long_codes: false };
            app.room_settings = Some(RoomSettings::default());
            app.status = "Connected to Beat.".into();
            app.sent_fps = Some(60.0);
            app.members = vec![
                member("m2", "Beat", Some(ConnState::Connected), Some(60.0), Some(0.2), Some(0), Some(38.0)),
                member("m3", "Mia", Some(ConnState::Connected), Some(57.0), Some(2.9), Some(4), Some(71.0)),
                member("m5", "Zoe", Some(ConnState::Connecting), None, None, None, None),
            ];
            if which == "buffer" {
                app.buffer_edit = Some(250);
            }
            if host {
                app.requests = vec![("m6".into(), "Kai".into())];
            }
            app.group_name = Some(collab::GROUP_SOURCE.into());
            let layer = |id: &str, label: &str, visible: bool, is_me: bool| GroupLayer {
                id: id.into(),
                label: label.into(),
                source: format!("Collab - {label}"),
                visible,
                is_me,
            };
            // "room" shows the group source switched on; "guest" shows it off.
            app.group_enabled = Some(Arc::new(std::sync::atomic::AtomicBool::new(host)));
            app.group_layers = Some(Arc::new(Mutex::new(vec![
                layer("m3", "Mia", true, false),
                layer("m2", "Beat", true, false),
                layer("me", "You (Ben)", false, true),
            ])));
            if which == "remove" {
                app.confirm_remove = Some("m3".into()); // "are you sure?" for Mia
            }
            match which {
                "about" => app.dialogs.push((Dialog::About, None)),
                "howto" => app.dialogs.push((Dialog::HowTo, None)),
                _ => {}
            }
        }
        _ => {} // "setup": the normal start screen
    }
}

fn member(
    id: &str,
    name: &str,
    state: Option<ConnState>,
    fps: Option<f64>,
    loss: Option<f64>,
    unrepaired: Option<u32>,
    rtt: Option<f64>,
) -> Member {
    Member {
        id: id.into(),
        name: name.into(),
        state,
        source: Some(format!("Collab - {name}")),
        fps,
        loss_percent: loss,
        unrepaired,
        round_trip_ms: rtt,
    }
}
