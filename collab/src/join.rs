//! `collab join "<Spout sender>"`: join a collab using an invite code.

use gstreamer as gst;
use gstreamer_webrtc as gst_webrtc;

use crate::host::open_avatar;
use crate::peer::{self, Error};
use crate::session::Session;
use crate::{code, console};

/// `my_name` = how the host will see us (their Spout source becomes "Collab - <my_name>").
pub fn run(sender_name: &str, my_name: &str) -> Result<(), Error> {
    let grabber = open_avatar(sender_name)?;

    // ---- Invite code: tells us who we're connecting to ----
    let invite = console::read_code("Paste the invite code: ")?;

    gst::init()?;
    let mut session = Session::start(grabber.width as usize, grabber.height as usize)?;
    session.set_partner(&invite.name);

    // ---- Answer with a reply code ----
    println!("Connecting to {}...", invite.name);
    peer::apply_remote(&session.webrtc, gst_webrtc::WebRTCSDPType::Offer, &invite)?;
    println!("Preparing reply code (finding your network addresses)...");
    let local_sdp = peer::create_local(&session.webrtc, gst_webrtc::WebRTCSDPType::Answer)?;
    let reply = code::encode(&session.take_gatherer().into_code(my_name, local_sdp));
    console::show_code("REPLY", &reply);
    println!("Send this reply code back to {}.", invite.name);

    session.run(sender_name, grabber)
}
