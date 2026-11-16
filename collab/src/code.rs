//! Invite / reply codes: the text people copy-paste to connect.
//!
//! A code carries everything the other side needs to reach you directly:
//!   * `sdp`        — WebRTC's "session description": what video we send, encryption keys...
//!   * `candidates` — network addresses we can be reached at (local, and public via STUN)
//!   * `name`       — the sender's display name (used to name the Spout source in OBS)
//!
//! To keep it short enough to paste: JSON text → compressed (deflate) → base64 text.

use std::io::{Read, Write};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use flate2::Compression;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use serde::{Deserialize, Serialize};

/// Every code starts with this, so we can recognise (and later version) them.
const PREFIX: &str = "VTC1-";

/// `#[derive(Serialize, Deserialize)]` lets serde turn this struct into JSON and back.
#[derive(Serialize, Deserialize, Debug)]
pub struct Code {
    pub name: String,
    pub sdp: String,
    pub candidates: Vec<(u32, String)>, // (media line index, candidate text)
}

pub fn encode(code: &Code) -> String {
    let json = serde_json::to_vec(code).expect("a Code always converts to JSON");
    let mut deflate = DeflateEncoder::new(Vec::new(), Compression::best());
    deflate.write_all(&json).expect("writing to memory can't fail");
    let compressed = deflate.finish().expect("writing to memory can't fail");
    format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(compressed))
}

/// `Result<Code, String>` = either the decoded code, or a readable error message.
pub fn decode(text: &str) -> Result<Code, String> {
    // People often paste extra spaces or line breaks; remove all whitespace.
    let text: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    // Skip anything before the prefix (invisible characters, a stray label...).
    let start = text
        .find(PREFIX)
        .ok_or("that doesn't look like a VTube Collab code (should start with VTC1-)")?;
    let body = &text[start + PREFIX.len()..];
    let compressed = URL_SAFE_NO_PAD
        .decode(body)
        .map_err(|_| "the code is damaged (maybe cut off while copying?)")?;
    let mut json = Vec::new();
    DeflateDecoder::new(&compressed[..])
        .read_to_end(&mut json)
        .map_err(|_| "the code is damaged (maybe cut off while copying?)")?;
    serde_json::from_slice(&json).map_err(|e| format!("the code couldn't be read: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_then_decode() {
        let code = Code {
            name: "Avatar Live".into(),
            sdp: "v=0\r\no=- 1 0 IN IP4 0.0.0.0\r\n".into(),
            candidates: vec![(0, "candidate:1 1 UDP 2015363327 192.168.1.5 50000 typ host".into())],
        };
        let text = encode(&code);
        assert!(text.starts_with(PREFIX));
        // Pasting with an invisible marker in front and a stray line break still works.
        let back = decode(&format!("\u{feff} {}\n{} ", &text[..20], &text[20..])).unwrap();
        assert_eq!(back.name, code.name);
        assert_eq!(back.sdp, code.sdp);
        assert_eq!(back.candidates, code.candidates);
    }
}
