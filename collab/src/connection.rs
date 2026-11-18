//! A two-person collab using copy-paste long codes (no room server needed).
//! This is the fallback when room codes can't be used; rooms (room.rs) are the normal way.
//!
//!   Host:  Connection::host(...)  → invite code   (send to partner)
//!          conn.accept_reply(...) ← reply code    (from partner)
//!          conn.run(stop)           sends/receives until `stop` is set
//!
//!   Guest: Connection::join(..., invite code) → reply code (send back)
//!          conn.run(stop)

use std::sync::atomic::AtomicBool;

use gstreamer as gst;
use gstreamer_webrtc as gst_webrtc;

use vtc_core::gpu::Packer;
use vtc_core::spout;

use crate::code;
use crate::events::{Event, EventSink};
use crate::group::GroupLayers;
use crate::peer::{self, Error};
use crate::session::{Role, Session};

/// The one partner's id in a two-person connection.
const PARTNER: &str = "partner";

pub struct Connection {
    session: Session,
    packer: Packer,
    sender_name: String,
    my_name: String,
}

impl Connection {
    /// Start hosting: opens the avatar, starts the pipeline, returns the invite code.
    pub fn host(sender_name: &str, my_name: &str, events: EventSink) -> Result<(Connection, String), Error> {
        let packer = open_avatar(sender_name, TWO_PERSON)?;
        gst::init()?;
        let mut session = Session::start(TWO_PERSON, (packer.source_width, packer.source_height), events.clone())?;
        session.add_partner(PARTNER, "partner", Role::Offerer)?;

        events(Event::Info("Preparing invite code (finding your network addresses)...".into()));
        let webrtc = session.partner_webrtc(PARTNER).expect("just added");
        let local_sdp = peer::create_local(&webrtc, gst_webrtc::WebRTCSDPType::Offer)?;
        let gatherer = session.take_gatherer(PARTNER).expect("just added");
        let invite = code::encode(&gatherer.into_code(my_name, local_sdp, &events));

        let conn = Connection { session, packer, sender_name: sender_name.to_string(), my_name: my_name.to_string() };
        Ok((conn, invite))
    }

    /// Host only: apply the partner's reply code. Returns the partner's name.
    /// A badly pasted code returns an error and can simply be tried again.
    pub fn accept_reply(&mut self, reply_text: &str) -> Result<String, Error> {
        let reply = code::decode(reply_text)?;
        self.session.rename_partner(PARTNER, &reply.name);
        let webrtc = self.session.partner_webrtc(PARTNER).expect("added in host()");
        peer::apply_remote(&webrtc, gst_webrtc::WebRTCSDPType::Answer, &reply)?;
        Ok(reply.name)
    }

    /// Join using an invite code. Returns (connection, reply code, partner's name).
    pub fn join(
        sender_name: &str,
        my_name: &str,
        invite_text: &str,
        events: EventSink,
    ) -> Result<(Connection, String, String), Error> {
        // Check the code first, so a bad paste fails fast before anything starts.
        let invite = code::decode(invite_text)?;
        let packer = open_avatar(sender_name, TWO_PERSON)?;
        gst::init()?;
        let mut session = Session::start(TWO_PERSON, (packer.source_width, packer.source_height), events.clone())?;
        session.add_partner(PARTNER, &invite.name, Role::Answerer)?;

        let webrtc = session.partner_webrtc(PARTNER).expect("just added");
        peer::apply_remote(&webrtc, gst_webrtc::WebRTCSDPType::Offer, &invite)?;
        events(Event::Info("Preparing reply code (finding your network addresses)...".into()));
        let local_sdp = peer::create_local(&webrtc, gst_webrtc::WebRTCSDPType::Answer)?;
        let gatherer = session.take_gatherer(PARTNER).expect("just added");
        let reply = code::encode(&gatherer.into_code(my_name, local_sdp, &events));

        let conn = Connection { session, packer, sender_name: sender_name.to_string(), my_name: my_name.to_string() };
        Ok((conn, reply, invite.name))
    }

    /// The group source's layer list. The app window keeps this handle to reorder
    /// and hide layers while the collab runs.
    pub fn group_layers(&self) -> GroupLayers {
        self.session.group_layers.clone()
    }

    /// The group source's on/off switch (off by default).
    pub fn group_enabled(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        self.session.group_enabled.clone()
    }

    /// Send and receive until `stop` becomes true. Cleans up (removes the Spout sources) on exit.
    pub fn run(self, stop: &AtomicBool) -> Result<(), Error> {
        self.session.run(&self.sender_name, &self.my_name, self.packer, stop, |_| Ok(()))
    }
}

/// Open our avatar's Spout2 sender for GPU packing at `transmit` size, with a helpful
/// message if it isn't running.
pub(crate) fn open_avatar(sender_name: &str, transmit: (u32, u32)) -> Result<Packer, Error> {
    let info = spout::read_sender_info(sender_name).ok_or_else(|| {
        format!("No Spout2 source called \"{sender_name}\". Is your VTubing app running with Spout2 output on?")
    })?;
    let packer = Packer::open(&info, transmit).map_err(|e| format!("Couldn't open your avatar: {}", e.message()))?;
    crate::log::info(&format!("Avatar opened on graphics card: {}", vtc_core::grab::card_in_use()));
    Ok(packer)
}

/// Two-person (long-code) collabs always send full 1080p.
const TWO_PERSON: (u32, u32) = (1920, 1080);
