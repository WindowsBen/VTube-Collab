//! A collab room for 2–20 people, joined with one room code (server: handshake/src/room_v2.js).
//!
//!   host:  Room::host(...)        → room code to share
//!   guest: Room::join(..., code)  → waits for the host to approve
//!   both:  room.run(stop, commands)  until `stop` is set
//!
//! How partners connect: when someone is let in, the room tells them who's already
//! there ("welcome"), and tells everyone else about them ("member_joined"). The
//! newcomer then makes an "offer" to each existing member, through the room; each
//! member answers. Every pair of people ends up with its own direct connection.

use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, Sender};

use gstreamer as gst;
use gstreamer_webrtc as gst_webrtc;

use vtc_core::gpu::Packer;

use crate::code;
use crate::connection::open_avatar;
use crate::events::{Event, EventSink, PeerId};
use crate::group::GroupLayers;
use crate::peer::{self, Error};
use crate::rooms::{self, RoomError};
use crate::session::{Role, Session};
use crate::signal::{self, FromRoom, RoomLink, RoomSettings, ToRoom};

/// What the user decides about someone asking to join (host only).
pub enum RoomCommand {
    Approve(PeerId),
    Decline(PeerId),
}

/// Room happenings, for the app's member list and join requests.
#[derive(Debug, Clone)]
pub enum RoomEvent {
    /// Waiting for the host to let us in.
    Waiting,
    /// We're in. `host` = we approve newcomers. `settings` = the room's settings.
    Admitted { host: bool, settings: RoomSettings },
    JoinRequest { id: PeerId, name: String },
    JoinCancelled { id: PeerId },
    MemberJoined { id: PeerId, name: String },
    MemberLeft { id: PeerId, name: String },
    /// The host left; we approve newcomers now.
    BecameHost,
    /// Our connection to the room server dropped. People already connected stay
    /// connected (video doesn't go through the server), but nobody new can join.
    ServerLost,
}

pub struct Room {
    link: RoomLink,
    session: Session,
    packer: Packer,
    sender_name: String,
    my_name: String,
    events: EventSink,
    room_events: std::sync::Arc<dyn Fn(RoomEvent) + Send + Sync>,
}

impl Room {
    /// Create a room with `settings` (people limit, receive buffer). Returns the room and its code.
    pub fn host(
        sender_name: &str,
        my_name: &str,
        settings: RoomSettings,
        events: EventSink,
        room_events: std::sync::Arc<dyn Fn(RoomEvent) + Send + Sync>,
    ) -> Result<(Room, String), Error> {
        let packer = open_avatar(sender_name)?;
        let (code, token) = signal::create_room(settings)?;
        let link = RoomLink::connect(&code, my_name, Some(&token))?;
        let room = Self::start(link, packer, sender_name, my_name, events, room_events)?;
        Ok((room, code))
    }

    /// Ask to join room `code` ("MINTY-OTTER-42", or typed loosely). The host must approve.
    pub fn join(
        sender_name: &str,
        my_name: &str,
        code: &str,
        events: EventSink,
        room_events: std::sync::Arc<dyn Fn(RoomEvent) + Send + Sync>,
    ) -> Result<Room, Error> {
        let code = rooms::normalise(code).ok_or("that isn't a room code (like MINTY-OTTER-42)")?;
        let packer = open_avatar(sender_name)?;
        let link = RoomLink::connect(&code, my_name, None).map_err(|e| match e {
            RoomError::NotFound => "that room code doesn't exist (or the room has closed)".into(),
            other => Error::from(other),
        })?;
        Self::start(link, packer, sender_name, my_name, events, room_events)
    }

    fn start(
        link: RoomLink,
        packer: Packer,
        sender_name: &str,
        my_name: &str,
        events: EventSink,
        room_events: std::sync::Arc<dyn Fn(RoomEvent) + Send + Sync>,
    ) -> Result<Room, Error> {
        gst::init()?;
        let session = Session::start(packer.width as usize, packer.height as usize, events.clone())?;
        Ok(Room {
            link,
            session,
            packer,
            sender_name: sender_name.to_string(),
            my_name: my_name.to_string(),
            events,
            room_events,
        })
    }

    /// The group source's layer list (for the app's layers panel).
    pub fn group_layers(&self) -> GroupLayers {
        self.session.group_layers.clone()
    }

    /// The group source's on/off switch (off by default).
    pub fn group_enabled(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        self.session.group_enabled.clone()
    }

    /// Run the collab until `stop` is set. `commands` carries the host's
    /// approve/decline decisions from the app.
    pub fn run(self, stop: &AtomicBool, commands: Receiver<RoomCommand>) -> Result<(), Error> {
        let Room { link, session, packer, sender_name, my_name, events, room_events } = self;
        let out = link.sender();
        let mut server_lost = false;

        session.run(&sender_name, &my_name, packer, stop, |s| {
            while let Ok(cmd) = commands.try_recv() {
                link.send(match cmd {
                    RoomCommand::Approve(id) => ToRoom::Approve { id },
                    RoomCommand::Decline(id) => ToRoom::Decline { id },
                });
            }
            while let Ok(msg) = link.incoming.try_recv() {
                match msg {
                    FromRoom::Waiting => room_events(RoomEvent::Waiting),
                    FromRoom::Welcome { host, members, settings, .. } => {
                        // Everyone in the room uses the host's buffer setting.
                        s.set_receive_buffer(settings.buffer_ms);
                        room_events(RoomEvent::Admitted { host, settings });
                        // We're the newcomer: offer a connection to everyone already here.
                        for m in members {
                            s.add_partner(&m.id, &m.name, Role::Offerer)?;
                            room_events(RoomEvent::MemberJoined { id: m.id.clone(), name: m.name.clone() });
                            start_offer(s, &m.id, &my_name, &out, &events);
                        }
                    }
                    FromRoom::Declined => return Err("the host didn't let you in".into()),
                    FromRoom::RoomFull => return Err("the room is full".into()),
                    FromRoom::JoinRequest { id, name } => room_events(RoomEvent::JoinRequest { id, name }),
                    FromRoom::JoinCancelled { id } => room_events(RoomEvent::JoinCancelled { id }),
                    FromRoom::MemberJoined { id, name } => {
                        // A newcomer: they'll send us an offer; get ready to answer.
                        s.add_partner(&id, &name, Role::Answerer)?;
                        room_events(RoomEvent::MemberJoined { id, name });
                    }
                    FromRoom::MemberLeft { id } => {
                        let name = s.partner_name(&id).unwrap_or_default();
                        s.remove_partner(&id);
                        room_events(RoomEvent::MemberLeft { id, name });
                    }
                    FromRoom::YouAreHost => room_events(RoomEvent::BecameHost),
                    FromRoom::Signal { from, data } => handle_signal(s, &from, &data, &my_name, &out, &events),
                    FromRoom::Disconnected => {
                        if !server_lost {
                            server_lost = true;
                            room_events(RoomEvent::ServerLost);
                        }
                    }
                }
            }
            Ok(())
        })
    }
}

/// Newcomer → existing member: create an offer and send it through the room.
/// Runs on its own thread, because finding network addresses can take a few seconds
/// and video to everyone else must keep flowing meanwhile.
fn start_offer(s: &mut Session, id: &str, my_name: &str, out: &Sender<ToRoom>, events: &EventSink) {
    let (Some(webrtc), Some(gatherer)) = (s.partner_webrtc(id), s.take_gatherer(id)) else { return };
    let (id, my_name, out, events) = (id.to_string(), my_name.to_string(), out.clone(), events.clone());
    std::thread::spawn(move || {
        let result = (|| -> Result<(), Error> {
            let sdp = peer::create_local(&webrtc, gst_webrtc::WebRTCSDPType::Offer)?;
            let data = code::encode(&gatherer.into_code(&my_name, sdp, &events));
            let _ = out.send(ToRoom::Signal { to: id.clone(), data });
            Ok(())
        })();
        if let Err(e) = result {
            events(Event::Warning(format!("couldn't start connecting to a partner: {e}")));
        }
    });
}

/// A connection code from partner `from`: their offer (we answer) or their answer.
fn handle_signal(s: &mut Session, from: &str, data: &str, my_name: &str, out: &Sender<ToRoom>, events: &EventSink) {
    let Some(role) = s.partner_role(from) else { return };
    let Some(webrtc) = s.partner_webrtc(from) else { return };
    let code = match code::decode(data) {
        Ok(c) => c,
        Err(e) => return events(Event::Warning(format!("bad connection code from a partner: {e}"))),
    };
    let gatherer = if role == Role::Answerer { s.take_gatherer(from) } else { None };
    let (from, my_name, out, events) = (from.to_string(), my_name.to_string(), out.clone(), events.clone());

    std::thread::spawn(move || {
        let result = (|| -> Result<(), Error> {
            match role {
                // We made the offer; this is their answer.
                Role::Offerer => peer::apply_remote(&webrtc, gst_webrtc::WebRTCSDPType::Answer, &code)?,
                // They made an offer; answer it.
                Role::Answerer => {
                    let gatherer = gatherer.ok_or("already answered this partner")?;
                    peer::apply_remote(&webrtc, gst_webrtc::WebRTCSDPType::Offer, &code)?;
                    let sdp = peer::create_local(&webrtc, gst_webrtc::WebRTCSDPType::Answer)?;
                    let data = code::encode(&gatherer.into_code(&my_name, sdp, &events));
                    let _ = out.send(ToRoom::Signal { to: from.clone(), data });
                }
            }
            Ok(())
        })();
        if let Err(e) = result {
            events(Event::Warning(format!("couldn't connect to a partner: {e}")));
        }
    });
}
