//! vtc-core: code shared by every VTube Collab program.
//!
//!   spout      — reading Spout2's list of senders and their details
//!   grab       — capturing frames from a Spout2 sender into memory (used by spout-probe)
//!   sender     — publishing our own Spout2 sender (for OBS)
//!   gpu        — packing colour + alpha side by side (so transparency survives H.264)
//!                and unpacking it again, on the graphics card
//!   compositor — the group source: several Spout sources stacked into one

pub mod compositor;
pub mod gpu;
pub mod grab;
pub mod sender;
pub mod spout;
