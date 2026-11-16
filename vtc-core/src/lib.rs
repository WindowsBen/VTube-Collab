//! vtc-core: code shared by every VTube Collab program.
//!
//!   spout   — reading Spout2's list of senders and their details
//!   grab    — capturing frames from a Spout2 sender (GPU → memory)
//!   sender  — publishing our own Spout2 sender (memory → GPU → OBS)
//!   packing — colour + alpha side by side, so transparency survives H.264

pub mod grab;
pub mod packing;
pub mod sender;
pub mod spout;
