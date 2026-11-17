//! Live connection to a v2 room on the handshake server (handshake/src/room_v2.js).
//!
//! A background thread keeps a WebSocket open to the room: messages from the room
//! arrive on `incoming`; messages we `send` go out. The thread also sends a small
//! "ping" every 25 s so the connection isn't dropped as idle (pings are free).

use std::io::ErrorKind;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use crate::rooms::{server_url, RoomError};

#[derive(Deserialize, Debug, Clone)]
pub struct MemberInfo {
    pub id: String,
    pub name: String,
}

/// Settings the host chose for the room. Everyone in the room uses the same ones.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct RoomSettings {
    /// Most people allowed in the room, counting the host (2–100).
    pub max_people: u32,
    /// Receive buffer for incoming video, in milliseconds (20–500).
    pub buffer_ms: u32,
}

impl Default for RoomSettings {
    fn default() -> RoomSettings {
        RoomSettings { max_people: 20, buffer_ms: crate::session::DEFAULT_BUFFER_MS }
    }
}

/// Messages from the room. `#[serde(tag = "type")]` maps {"type":"member_left",...} to `MemberLeft`.
#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromRoom {
    Welcome {
        you: String,
        host: bool,
        members: Vec<MemberInfo>,
        /// Missing from older servers → defaults.
        #[serde(default)]
        settings: RoomSettings,
    },
    Waiting,
    Declined,
    RoomFull,
    JoinRequest { id: String, name: String },
    JoinCancelled { id: String },
    MemberJoined { id: String, name: String },
    MemberLeft { id: String },
    YouAreHost,
    Signal { from: String, data: String },
    /// Not from the server: our connection to the room ended.
    #[serde(skip)]
    Disconnected,
}

/// Messages to the room.
#[derive(Serialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToRoom {
    Approve { id: String },
    Decline { id: String },
    Signal { to: String, data: String },
}

pub struct RoomLink {
    pub incoming: Receiver<FromRoom>,
    outgoing: Sender<ToRoom>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// Host: create a v2 room with `settings`. Returns (room code, host token).
pub fn create_room(settings: RoomSettings) -> Result<(String, String), RoomError> {
    #[derive(Deserialize)]
    struct Created {
        code: String,
        token: String,
    }
    let base = server_url().ok_or_else(|| RoomError::Unavailable("no room server set up".into()))?;
    let url = format!("{}/v2/rooms", base.trim_end_matches('/'));
    let body = serde_json::to_string(&settings).expect("settings convert to JSON");
    let mut response = ureq::post(&url).send(&body).map_err(|e| RoomError::Unavailable(e.to_string()))?;
    let text = response.body_mut().read_to_string().map_err(|e| RoomError::Other(e.to_string()))?;
    // An older room server answers with plain text instead of a new room.
    let created: Created = serde_json::from_str(&text)
        .map_err(|_| RoomError::Unavailable("the room server doesn't support group rooms yet".into()))?;
    Ok((created.code, created.token))
}

impl RoomLink {
    /// Connect to room `code` as `name`. The host passes its `token`.
    pub fn connect(code: &str, name: &str, token: Option<&str>) -> Result<RoomLink, RoomError> {
        let base = server_url().ok_or_else(|| RoomError::Unavailable("no room server set up".into()))?;
        let ws_base = base.trim_end_matches('/').replacen("https://", "wss://", 1).replacen("http://", "ws://", 1);
        let mut url = format!("{ws_base}/v2/rooms/{code}/ws?name={}", url_encode(name));
        if let Some(t) = token {
            url.push_str(&format!("&token={}", url_encode(t)));
        }

        let (mut socket, _) = tungstenite::connect(url.as_str()).map_err(|e| match e {
            tungstenite::Error::Http(r) if r.status().as_u16() == 404 => RoomError::NotFound,
            other => RoomError::Unavailable(other.to_string()),
        })?;
        // Short read timeout, so the thread can also send while waiting for messages.
        set_read_timeout(&mut socket, Duration::from_millis(50));

        let (in_tx, incoming) = mpsc::channel();
        let (outgoing, out_rx) = mpsc::channel::<ToRoom>();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut last_ping = Instant::now();
            loop {
                if thread_stop.load(Ordering::Relaxed) {
                    let _ = socket.close(None);
                    let _ = socket.flush();
                    return;
                }
                match socket.read() {
                    Ok(Message::Text(text)) => {
                        if let Ok(msg) = serde_json::from_str::<FromRoom>(text.as_str()) {
                            let _ = in_tx.send(msg);
                        }
                    }
                    Ok(Message::Close(_)) => break,
                    Ok(_) => {} // pongs etc.
                    Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                    Err(_) => break,
                }
                while let Ok(msg) = out_rx.try_recv() {
                    let json = serde_json::to_string(&msg).expect("ToRoom converts to JSON");
                    if socket.send(Message::text(json)).is_err() {
                        break;
                    }
                }
                if last_ping.elapsed() > Duration::from_secs(25) {
                    let _ = socket.send(Message::Ping(Default::default()));
                    last_ping = Instant::now();
                }
            }
            let _ = in_tx.send(FromRoom::Disconnected);
        });

        Ok(RoomLink { incoming, outgoing, stop, thread: Some(thread) })
    }

    /// A handle for sending to the room from other threads (e.g. negotiation threads).
    pub fn sender(&self) -> Sender<ToRoom> {
        self.outgoing.clone()
    }

    pub fn send(&self, msg: ToRoom) {
        let _ = self.outgoing.send(msg);
    }
}

impl Drop for RoomLink {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn set_read_timeout(socket: &mut WebSocket<MaybeTlsStream<TcpStream>>, timeout: Duration) {
    let tcp = match socket.get_mut() {
        MaybeTlsStream::Plain(s) => s,
        MaybeTlsStream::Rustls(s) => &mut s.sock,
        _ => return,
    };
    let _ = tcp.set_read_timeout(Some(timeout));
}

/// Make text safe to put in a web address ("Ben & Co" → "Ben%20%26%20Co").
fn url_encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}
