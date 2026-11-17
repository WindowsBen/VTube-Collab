//! Short room codes (e.g. MINTY-OTTER-42) through the handshake server (handshake/).
//!
//! The server is just a mailbox for the two long connection codes:
//!   host:  create(invite) → room code   …later…   poll_reply(room) → reply
//!   guest: take_invite(room) → invite   …then…    put_reply(room, reply)
//! If the server can't be reached, the app falls back to copy-pasting long codes.

use std::fmt;
use std::time::Duration;

use ureq::Agent;

/// The handshake server's address (handshake/, deployed on Cloudflare Workers).
/// The `VTC_SERVER` environment variable overrides it, e.g. for a local test server.
const DEFAULT_SERVER: Option<&str> = Some("https://vtube-collab.mpavatars-relay.workers.dev");

pub fn server_url() -> Option<String> {
    std::env::var("VTC_SERVER").ok().or(DEFAULT_SERVER.map(String::from))
}

#[derive(Debug)]
pub enum RoomError {
    /// No handshake server configured, or it couldn't be reached.
    Unavailable(String),
    /// The room code doesn't exist (typo) or has expired.
    NotFound,
    /// Someone already joined this room.
    Taken,
    Other(String),
}

impl fmt::Display for RoomError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            RoomError::Unavailable(why) => write!(f, "the room server can't be reached ({why})"),
            RoomError::NotFound => write!(f, "that room code doesn't exist or has expired"),
            RoomError::Taken => write!(f, "someone already joined that room"),
            RoomError::Other(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for RoomError {}

/// Clean up a typed room code: "minty otter 42" → "MINTY-OTTER-42".
/// Returns `None` if the text can't be a room code.
pub fn normalise(text: &str) -> Option<String> {
    // Drop anything that can't be part of a room code (invisible characters, quotes...).
    let text: String = text
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || c.is_whitespace() || *c == '-' || *c == '_')
        .collect();
    let parts: Vec<String> = text
        .split(|c: char| c.is_whitespace() || c == '-' || c == '_')
        .filter(|p| !p.is_empty())
        .map(|p| p.to_uppercase())
        .collect();
    let valid = parts.len() == 3
        && parts[..2].iter().all(|w| w.chars().all(|c| c.is_ascii_alphabetic()))
        && parts[2].len() <= 2
        && parts[2].chars().all(|c| c.is_ascii_digit());
    valid.then(|| format!("{}-{}-{:0>2}", parts[0], parts[1], parts[2]))
}

/// A web client that reports HTTP status codes to us instead of treating them as errors.
fn agent() -> Agent {
    Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .into()
}

/// Do one request. Returns (status code, body text).
fn request(method: &str, path: &str, body: Option<&str>) -> Result<(u16, String), RoomError> {
    let base = server_url().ok_or_else(|| RoomError::Unavailable("no room server set up".into()))?;
    let url = format!("{}{path}", base.trim_end_matches('/'));
    let agent = agent();
    let result = match (method, body) {
        ("POST", Some(b)) => agent.post(&url).send(b),
        ("PUT", Some(b)) => agent.put(&url).send(b),
        _ => agent.get(&url).call(),
    };
    let mut response = result.map_err(|e| RoomError::Unavailable(e.to_string()))?;
    let status = response.status().as_u16();
    let text = response.body_mut().read_to_string().unwrap_or_default();
    if status >= 500 {
        return Err(RoomError::Unavailable(format!("server error {status}")));
    }
    Ok((status, text))
}

/// Host: put our invite code in a new room. Returns the room code.
pub fn create(invite: &str) -> Result<String, RoomError> {
    match request("POST", "/rooms", Some(invite))? {
        (201, code) => Ok(code.trim().to_string()),
        (status, text) => Err(RoomError::Other(format!("couldn't create a room ({status} {text})"))),
    }
}

/// Guest: collect the host's invite code from the room.
pub fn take_invite(room: &str) -> Result<String, RoomError> {
    match request("GET", &format!("/rooms/{room}/invite"), None)? {
        (200, invite) => Ok(invite),
        (409, _) => Err(RoomError::Taken),
        _ => Err(RoomError::NotFound),
    }
}

/// Guest: leave our reply code in the room for the host.
pub fn put_reply(room: &str, reply: &str) -> Result<(), RoomError> {
    match request("PUT", &format!("/rooms/{room}/reply"), Some(reply))? {
        (200, _) => Ok(()),
        (409, _) => Err(RoomError::Taken),
        _ => Err(RoomError::NotFound),
    }
}

/// Host: check whether the guest's reply has arrived. `Ok(None)` = not yet.
pub fn poll_reply(room: &str) -> Result<Option<String>, RoomError> {
    match request("GET", &format!("/rooms/{room}/reply"), None)? {
        (200, reply) => Ok(Some(reply)),
        (204, _) => Ok(None),
        _ => Err(RoomError::NotFound),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_code_cleanup() {
        assert_eq!(normalise("minty otter 42").as_deref(), Some("MINTY-OTTER-42"));
        assert_eq!(normalise(" Minty-Otter-7 ").as_deref(), Some("MINTY-OTTER-07"));
        assert_eq!(normalise("\u{feff}\"minty otter 42\"").as_deref(), Some("MINTY-OTTER-42"));
        assert_eq!(normalise("VTC1-abc"), None);
        assert_eq!(normalise("MINTY-OTTER"), None);
        assert_eq!(normalise("MINTY-OTTER-123"), None);
    }
}
