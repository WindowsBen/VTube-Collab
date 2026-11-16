//! `collab host "<Spout sender>"`: start a collab and invite a partner.

use gstreamer as gst;
use gstreamer_webrtc as gst_webrtc;

use vtc_core::grab::Grabber;
use vtc_core::spout;

use crate::peer::{self, Error};
use crate::session::Session;
use crate::{code, console};

/// `my_name` = how the partner will see us (their Spout source becomes "Collab - <my_name>").
pub fn run(sender_name: &str, my_name: &str) -> Result<(), Error> {
    let grabber = open_avatar(sender_name)?;
    gst::init()?;
    let mut session = Session::start(grabber.width as usize, grabber.height as usize)?;

    // ---- Invite code ----
    println!("Preparing invite code (finding your network addresses)...");
    let local_sdp = peer::create_local(&session.webrtc, gst_webrtc::WebRTCSDPType::Offer)?;
    let invite = code::encode(&session.take_gatherer().into_code(my_name, local_sdp));
    console::show_code("INVITE", &invite);
    println!("Send this invite code to your partner, then paste their reply code here.");

    // ---- Reply code ----
    let reply = console::read_code("Reply code: ")?;
    session.set_partner(&reply.name);
    println!("Connecting to {}...", reply.name);
    peer::apply_remote(&session.webrtc, gst_webrtc::WebRTCSDPType::Answer, &reply)?;

    session.run(sender_name, grabber)
}

/// Open our avatar's Spout2 sender, with a helpful message if it isn't running.
pub fn open_avatar(sender_name: &str) -> Result<Grabber, Error> {
    let info = spout::read_sender_info(sender_name).ok_or_else(|| {
        format!("No Spout2 sender called \"{sender_name}\". Is your VTubing app running with Spout2 output on?")
    })?;
    Ok(Grabber::open(&info)?)
}
