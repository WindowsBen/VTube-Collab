//! Live connection to a v2 room on the handshake server (handshake/src/room_v2.js).
//!
//! A background thread keeps a WebSocket open to the room: messages from the room
//! arrive on `incoming`; messages we `send` go out. The thread also sends a small
//! "ping" every 20 s; the server answers "pong" without waking the room (free). If
//! no pong comes back for a while, the connection is treated as dead.
//!
//! If the connection drops after we're in the room, the thread reconnects by itself
//! and "resumes" our place (the room gave us a secret for that in "welcome"). Video
//! between people never goes through the server, so it keeps running meanwhile.

use std::io::ErrorKind;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::protocol::CloseFrame;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{HandshakeError, Message, WebSocket};

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
        /// Secret for getting our place back after a dropped connection (newer servers).
        #[serde(default)]
        resume: Option<String>,
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
    /// We reconnected and got our place back. Anything we missed follows.
    Resumed {
        #[serde(default)]
        host: bool,
    },
    /// We reconnected, but the room had already let us go.
    ResumeFailed,
    /// The host removed us from the room.
    Removed,
    /// Not from the server: the connection dropped; trying to reconnect.
    #[serde(skip)]
    Reconnecting { why: String },
    /// Not from the server: our connection to the room ended for good.
    #[serde(skip)]
    Disconnected { why: String },
}

/// Messages to the room.
#[derive(Serialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToRoom {
    Approve { id: String },
    Decline { id: String },
    /// Host only: take someone out of the room.
    Remove { id: String },
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

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

/// How often we ping, and how long without a pong before the connection counts as dead.
const PING_EVERY: Duration = Duration::from_secs(20);
const PONG_TIMEOUT: Duration = Duration::from_secs(60);
/// How long to keep trying to reconnect. The server keeps our place for 45 s.
const RECONNECT_FOR: Duration = Duration::from_secs(40);

impl RoomLink {
    /// Connect to room `code` as `name`. The host passes its `token`.
    pub fn connect(code: &str, name: &str, token: Option<&str>) -> Result<RoomLink, RoomError> {
        let base = server_url().ok_or_else(|| RoomError::Unavailable("no room server set up".into()))?;
        let ws_base = base.trim_end_matches('/').replacen("https://", "wss://", 1).replacen("http://", "ws://", 1);
        // v=3: we ping and can resume (the server only expects pings from such clients).
        let mut url = format!("{ws_base}/v2/rooms/{code}/ws?v=3&name={}", url_encode(name));
        if let Some(t) = token {
            url.push_str(&format!("&token={}", url_encode(t)));
        }
        let socket = open(&url)?;

        let (in_tx, incoming) = mpsc::channel();
        let (outgoing, out_rx) = mpsc::channel::<ToRoom>();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let thread = std::thread::spawn(move || run_link(socket, url, in_tx, out_rx, thread_stop));

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

/// The link thread: pass messages both ways, ping, and reconnect when needed.
fn run_link(first: Socket, url: String, in_tx: Sender<FromRoom>, out_rx: Receiver<ToRoom>, stop: Arc<AtomicBool>) {
    let mut socket = Some(first);
    // "id.secret", once the room has let us in.
    let mut resume: Option<String> = None;
    // Messages not sent yet (kept across a reconnect).
    let mut pending: std::collections::VecDeque<String> = Default::default();
    let mut last_ping = Instant::now();
    let mut last_pong = Instant::now();
    // Only an up-to-date server answers pings; until we've seen one pong, don't expect them.
    let mut pong_seen = false;
    // While reconnecting: since when, and when to try next.
    let mut down_since = Instant::now();
    let mut next_try = Instant::now();
    let mut attempt = 0u32;
    let mut why = String::new();

    loop {
        if stop.load(Ordering::Relaxed) {
            if let Some(ws) = socket.as_mut() {
                say_goodbye(ws);
            }
            return;
        }

        let Some(ws) = socket.as_mut() else {
            // ---- reconnecting ----
            if down_since.elapsed() > RECONNECT_FOR {
                let _ = in_tx.send(FromRoom::Disconnected { why });
                return;
            }
            if Instant::now() >= next_try {
                let resume_url = format!("{url}&resume={}", resume.as_deref().map(url_encode).unwrap_or_default());
                match open(&resume_url) {
                    Ok(ws) => {
                        crate::log::info("Room server: reconnected, resuming our place");
                        socket = Some(ws);
                        (last_ping, last_pong) = (Instant::now(), Instant::now());
                    }
                    Err(e) => {
                        crate::log::info(&format!("Room server: reconnect attempt failed: {e}"));
                        attempt += 1;
                        next_try = Instant::now() + Duration::from_secs([1, 2, 3, 5][attempt.min(3) as usize]);
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };

        // ---- connected ----
        let mut busy = false;
        let mut failed: Option<String> = None;

        // Read everything that has arrived.
        loop {
            match ws.read() {
                Ok(Message::Text(text)) => {
                    busy = true;
                    if text.as_str() == "pong" {
                        (last_pong, pong_seen) = (Instant::now(), true);
                        continue;
                    }
                    let Ok(msg) = serde_json::from_str::<FromRoom>(text.as_str()) else { continue };
                    match &msg {
                        FromRoom::Welcome { you, resume: Some(secret), .. } => resume = Some(format!("{you}.{secret}")),
                        // No reconnecting after these: our place is gone.
                        FromRoom::ResumeFailed | FromRoom::Removed => resume = None,
                        _ => {}
                    }
                    let _ = in_tx.send(msg);
                }
                Ok(Message::Close(frame)) => {
                    let reason = frame.map(|f| f.reason.to_string()).filter(|r| !r.is_empty());
                    failed = Some(format!("the server closed the connection ({})", reason.as_deref().unwrap_or("no reason given")));
                    break;
                }
                Ok(_) => busy = true, // protocol pings/pongs
                Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => {
                    failed = Some(e.to_string());
                    break;
                }
            }
        }

        // Send what's waiting. A message stays in `pending` until it's handed over.
        if failed.is_none() {
            while let Ok(msg) = out_rx.try_recv() {
                pending.push_back(serde_json::to_string(&msg).expect("ToRoom converts to JSON"));
            }
            if last_ping.elapsed() > PING_EVERY {
                last_ping = Instant::now();
                pending.push_front("ping".into());
            }
            while let Some(text) = pending.front() {
                match ws.send(Message::text(text.clone())) {
                    // WouldBlock = queued; it goes out with the next flush.
                    Ok(()) => {}
                    Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => {}
                    Err(e) => {
                        failed = Some(e.to_string());
                        break;
                    }
                }
                pending.pop_front();
                busy = true;
            }
            pending.retain(|t| t != "ping"); // a stale ping is pointless after a reconnect
        }
        if failed.is_none() {
            match ws.flush() {
                Ok(()) => {}
                Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => {}
                Err(e) => failed = Some(e.to_string()),
            }
        }
        if failed.is_none() && pong_seen && last_pong.elapsed() > PONG_TIMEOUT {
            failed = Some("no answer from the server for 60 s".into());
        }

        if let Some(reason) = failed {
            socket = None;
            if resume.is_some() {
                crate::log::warning(&format!("Room server connection dropped: {reason}"));
                let _ = in_tx.send(FromRoom::Reconnecting { why: reason.clone() });
                why = reason;
                (down_since, next_try, attempt) = (Instant::now(), Instant::now() + Duration::from_secs(1), 0);
                continue;
            }
            let _ = in_tx.send(FromRoom::Disconnected { why: reason });
            return;
        }
        if !busy {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// Leaving: tell the room on purpose ("normal" close), so it says goodbye to the
/// others at once instead of keeping our place for a reconnect.
fn say_goodbye(ws: &mut Socket) {
    let _ = ws.close(Some(CloseFrame { code: CloseCode::Normal, reason: "leave".into() }));
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        match ws.flush() {
            Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10))
            }
            _ => return,
        }
    }
}

/// Open a WebSocket to `url` (giving up on unreachable servers after 5 s), then
/// switch it to "non-blocking": reads return at once when nothing has arrived, so
/// one thread can both listen and send.
fn open(url: &str) -> Result<Socket, RoomError> {
    let unavailable = |e: &dyn std::fmt::Display| RoomError::Unavailable(e.to_string());
    let uri: tungstenite::http::Uri = url.parse().map_err(|e| unavailable(&e))?;
    let host = uri.host().ok_or_else(|| RoomError::Unavailable("bad server address".into()))?;
    let port = uri.port_u16().unwrap_or(if uri.scheme_str() == Some("wss") { 443 } else { 80 });
    let addrs: Vec<_> = (host, port).to_socket_addrs().map_err(|e| unavailable(&e))?.collect();
    let tcp = addrs
        .iter()
        .find_map(|a| TcpStream::connect_timeout(a, Duration::from_secs(5)).ok())
        .ok_or_else(|| RoomError::Unavailable("couldn't reach the room server".into()))?;
    // While connecting, wait at most 10 s for each answer.
    let _ = tcp.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = tcp.set_nodelay(true);

    let (mut socket, _) = tungstenite::client_tls(url, tcp).map_err(|e| match e {
        HandshakeError::Failure(tungstenite::Error::Http(r)) if r.status().as_u16() == 404 => RoomError::NotFound,
        HandshakeError::Failure(other) => unavailable(&other),
        HandshakeError::Interrupted(_) => RoomError::Unavailable("connection interrupted".into()),
    })?;
    let tcp = match socket.get_mut() {
        MaybeTlsStream::Plain(s) => s,
        MaybeTlsStream::Rustls(s) => &mut s.sock,
        _ => return Err(RoomError::Other("unexpected connection type".into())),
    };
    tcp.set_read_timeout(None).map_err(|e| unavailable(&e))?;
    tcp.set_nonblocking(true).map_err(|e| unavailable(&e))?;
    Ok(socket)
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
