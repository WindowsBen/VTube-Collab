//! Room codes (e.g. MINTY-OTTER-42): where the room server is, its errors, and
//! cleaning up a typed code. The live connection to a room is in signal.rs.

use std::fmt;

/// The room server's address (handshake/, deployed on Cloudflare Workers).
/// The `VTC_SERVER` environment variable overrides it, e.g. for a local test server.
const DEFAULT_SERVER: Option<&str> = Some("https://vtube-collab.mpavatars-relay.workers.dev");

pub fn server_url() -> Option<String> {
    std::env::var("VTC_SERVER").ok().or(DEFAULT_SERVER.map(String::from))
}

#[derive(Debug)]
pub enum RoomError {
    /// No room server configured, or it couldn't be reached.
    Unavailable(String),
    /// The room code doesn't exist (typo) or the room has closed.
    NotFound,
    Other(String),
}

impl fmt::Display for RoomError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            RoomError::Unavailable(why) => write!(f, "the room server can't be reached ({why})"),
            RoomError::NotFound => write!(f, "that room code doesn't exist or the room has closed"),
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
