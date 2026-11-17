//! A collab connection, step by step. Used by both the app window and the command line.
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
use crate::peer::{self, Error};
use crate::session::Session;

pub struct Connection {
    session: Session,
    packer: Packer,
    sender_name: String,
}

impl Connection {
    /// Start hosting: opens the avatar, starts the pipeline, returns the invite code.
    pub fn host(sender_name: &str, my_name: &str, events: EventSink) -> Result<(Connection, String), Error> {
        let packer = open_avatar(sender_name)?;
        gst::init()?;
        let mut session = Session::start(packer.width as usize, packer.height as usize, events.clone())?;

        events(Event::Info("Preparing invite code (finding your network addresses)...".into()));
        let local_sdp = peer::create_local(&session.webrtc, gst_webrtc::WebRTCSDPType::Offer)?;
        let invite = code::encode(&session.take_gatherer().into_code(my_name, local_sdp, &events));

        Ok((Connection { session, packer, sender_name: sender_name.to_string() }, invite))
    }

    /// Host only: apply the partner's reply code. Returns the partner's name.
    /// A badly pasted code returns an error and can simply be tried again.
    pub fn accept_reply(&mut self, reply_text: &str) -> Result<String, Error> {
        let reply = code::decode(reply_text)?;
        self.session.set_partner(&reply.name);
        peer::apply_remote(&self.session.webrtc, gst_webrtc::WebRTCSDPType::Answer, &reply)?;
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
        let packer = open_avatar(sender_name)?;
        gst::init()?;
        let mut session = Session::start(packer.width as usize, packer.height as usize, events.clone())?;
        session.set_partner(&invite.name);

        peer::apply_remote(&session.webrtc, gst_webrtc::WebRTCSDPType::Offer, &invite)?;
        events(Event::Info("Preparing reply code (finding your network addresses)...".into()));
        let local_sdp = peer::create_local(&session.webrtc, gst_webrtc::WebRTCSDPType::Answer)?;
        let reply = code::encode(&session.take_gatherer().into_code(my_name, local_sdp, &events));

        let conn = Connection { session, packer, sender_name: sender_name.to_string() };
        Ok((conn, reply, invite.name))
    }

    /// Send and receive until `stop` becomes true. Cleans up (removes the Spout source) on exit.
    pub fn run(self, stop: &AtomicBool) -> Result<(), Error> {
        self.session.run(&self.sender_name, self.packer, stop)
    }
}

/// Open our avatar's Spout2 sender for GPU packing, with a helpful message if it isn't running.
fn open_avatar(sender_name: &str) -> Result<Packer, Error> {
    let info = spout::read_sender_info(sender_name).ok_or_else(|| {
        format!("No Spout2 source called \"{sender_name}\". Is your VTubing app running with Spout2 output on?")
    })?;
    Ok(Packer::open(&info)?)
}
