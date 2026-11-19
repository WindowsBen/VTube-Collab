// Room server v2: rooms for 2–100 people, joined with one room code.
//
// Everyone in a room keeps a WebSocket (a live two-way connection) to the room
// while they're in the collab. The room:
//   * asks the host to approve each newcomer (Accept / Decline),
//   * tells everyone when someone joins or leaves,
//   * passes connection codes ("signals") between pairs of members, so each pair
//     can open its own direct connection. Video never goes through here.
//
// Messages are small JSON texts. Client → room:
//   {type:"approve", id}  {type:"decline", id}         (host only)
//   {type:"remove", id}                                (host only) take someone out of the room
//   {type:"set_buffer", buffer_ms}                     (host only) change everyone's receive buffer
//   {type:"signal", to, data}                          (approved members only)
//   "ping" (plain text)                                answered "pong" without waking the room
// Room → client:
//   {type:"welcome", you, host, members:[{id,name}], settings:{max_people, buffer_ms}, resume}
//                                                      you're in (members = who's already there;
//                                                      resume = secret for getting your place back)
//   {type:"settings", settings}                        the host changed the room's settings
//   {type:"waiting"}                                   waiting for the host to approve you
//   {type:"declined"} {type:"room_full"}               then the room closes your connection
//   {type:"join_request", id, name}                    (to host) someone wants to join
//   {type:"join_cancelled", id}                        (to host) they gave up waiting
//   {type:"member_joined", id, name}  {type:"member_left", id}
//   {type:"you_are_host"}                              the host left; you approve newcomers now
//   {type:"signal", from, data}
//   {type:"resumed", host} {type:"resume_failed"}      answer to a reconnect (see below)
//   {type:"removed"}                                   the host removed you; then the room closes your connection
//
// Dropped connections: if a member's connection drops without them saying goodbye
// (a normal close), their place is kept for 45 s. They can reconnect with
// ?resume=<id>.<secret> and carry on; messages meant for them meanwhile are kept
// and delivered then. Only after 45 s do the others hear "member_left".
//
// Up-to-date clients (?v=3) send "ping" every 20 s. One that has been silent for
// 70 s is treated as dropped (its connection is probably dead without the room
// having noticed). Older clients don't ping, so they're never checked.
//
// Uses Cloudflare's WebSocket "hibernation": while nobody is talking the room sleeps
// and costs nothing. Members who are in the room are stored ("m:<id>"), so a room
// that restarts still knows them; who each connection is is stored on the
// connection itself ("attachment").

import { DurableObject } from "cloudflare:workers";

// Room settings chosen by the host (limits enforced here, whatever a client asks for).
const PEOPLE_LIMITS = { min: 2, max: 100, default: 20 }; // counting the host
const BUFFER_LIMITS = { min: 20, max: 500, default: 120 }; // receive buffer, ms

function clampSetting(value, limits) {
  const n = Math.round(Number(value));
  return Number.isFinite(n) ? Math.min(limits.max, Math.max(limits.min, n)) : limits.default;
}

/// Room settings from the host's create request (anything missing → default).
export function roomSettings(body) {
  return {
    max_people: clampSetting(body?.max_people, PEOPLE_LIMITS),
    buffer_ms: clampSetting(body?.buffer_ms, BUFFER_LIMITS),
  };
}

const MAX_SIGNAL_BYTES = 16 * 1024;
const MAX_NAME_LENGTH = 32;
// An empty room (nobody in it) is deleted after 10 minutes.
const DEFAULT_EMPTY_LIFETIME_SECONDS = 10 * 60;
// How long a dropped member's place is kept for them to reconnect.
const GRACE_MS = 45 * 1000;
// A pinging client silent for this long counts as dropped.
const HEARTBEAT_TIMEOUT_MS = 70 * 1000;
// Most messages kept for one dropped member.
const MAX_KEPT_MESSAGES = 50;

export class RoomV2 extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    // Answer "ping" with "pong" without waking the room (free).
    ctx.setWebSocketAutoResponse(new WebSocketRequestResponsePair("ping", "pong"));
  }

  emptyLifetimeMs() {
    return (Number(this.env.ROOM_LIFETIME_SECONDS) || DEFAULT_EMPTY_LIFETIME_SECONDS) * 1000;
  }

  /// Host creates the room with `settings` (from roomSettings). Returns the host's
  /// secret token, or null if the code is taken.
  async create(settings) {
    if (await this.ctx.storage.get("room")) return null;
    const hostToken = crypto.randomUUID();
    await this.ctx.storage.put("room", { hostToken, nextId: 1, settings, emptySince: Date.now() });
    await this.scheduleAlarm();
    return hostToken;
  }

  // ---------- helpers ----------

  /// Open connections, with who they are. (Leaves out `except`, e.g. one that's closing.)
  members(except = null) {
    return this.ctx
      .getWebSockets()
      .filter((ws) => ws !== except)
      .map((ws) => ({ ws, info: ws.deserializeAttachment() }))
      .filter((m) => m.info);
  }

  /// The open connection of member `id`, if any.
  live(id) {
    return this.members().find((m) => m.info.approved && m.info.id === id);
  }

  /// People in the room (stored), by id: {id, name, secret, host, order, goneSince}.
  async records() {
    const list = await this.ctx.storage.list({ prefix: "m:" });
    return [...list.values()];
  }

  send(ws, message) {
    try {
      ws.send(JSON.stringify(message));
    } catch {
      // The connection is already closing; nothing to do.
    }
  }

  /// Send to member `id`; if their connection dropped, keep it for when they reconnect.
  async deliver(id, message) {
    const m = this.live(id);
    if (m) return this.send(m.ws, message);
    const rec = await this.ctx.storage.get(`m:${id}`);
    if (!rec) return;
    const kept = (await this.ctx.storage.get(`q:${id}`)) || [];
    if (kept.length < MAX_KEPT_MESSAGES) {
      kept.push(message);
      await this.ctx.storage.put(`q:${id}`, kept);
    }
  }

  /// Tell the host (whoever has `host` set) about everyone waiting to be let in.
  async sendWaitingTo(hostId) {
    for (const m of this.members()) {
      if (!m.info.approved) await this.deliver(hostId, { type: "join_request", id: m.info.id, name: m.info.name });
    }
  }

  /// Wake the room at the next deadline: a dropped member's grace running out, or
  /// an empty room's lifetime ending.
  async scheduleAlarm() {
    const room = await this.ctx.storage.get("room");
    if (!room) return;
    const deadlines = [];
    for (const r of await this.records()) if (r.goneSince) deadlines.push(r.goneSince + GRACE_MS);
    if (room.emptySince) deadlines.push(room.emptySince + this.emptyLifetimeMs());
    if (deadlines.length) await this.ctx.storage.setAlarm(Math.min(...deadlines));
    else await this.ctx.storage.deleteAlarm();
  }

  /// Member `id`'s connection dropped: keep their place for GRACE_MS.
  async dropped(id) {
    const rec = await this.ctx.storage.get(`m:${id}`);
    if (!rec || rec.goneSince) return;
    rec.goneSince = Date.now();
    await this.ctx.storage.put(`m:${id}`, rec);
    await this.scheduleAlarm();
  }

  /// Close up-to-date clients that stopped pinging (their connection is likely dead
  /// without us having noticed). They can still reconnect within the grace period.
  async sweep() {
    const now = Date.now();
    for (const m of this.members()) {
      if (!m.info.hb) continue;
      const pinged = this.ctx.getWebSocketAutoResponseTimestamp(m.ws)?.getTime() ?? 0;
      if (now - Math.max(pinged, m.info.since || 0) <= HEARTBEAT_TIMEOUT_MS) continue;
      m.ws.serializeAttachment(null);
      try {
        m.ws.close(4001, "no ping");
      } catch {}
      if (m.info.approved) await this.dropped(m.info.id);
      else await this.cancelWaiting(m.info.id);
    }
  }

  async cancelWaiting(id) {
    const host = (await this.records()).find((r) => r.host);
    if (host) await this.deliver(host.id, { type: "join_cancelled", id });
  }

  // ---------- new connections ----------

  async fetch(request) {
    const room = await this.ctx.storage.get("room");
    if (!room) return new Response("room not found or expired", { status: 404 });
    if (request.headers.get("Upgrade") !== "websocket") {
      return new Response("expected a WebSocket", { status: 426 });
    }
    await this.sweep();

    const url = new URL(request.url);
    const name = (url.searchParams.get("name") || "Guest").trim().slice(0, MAX_NAME_LENGTH) || "Guest";
    const hb = url.searchParams.get("v") === "3";
    const resume = url.searchParams.get("resume");

    const pair = new WebSocketPair();
    const [client, server] = Object.values(pair);
    this.ctx.acceptWebSocket(server);
    const answer = new Response(null, { status: 101, webSocket: client });

    // ---- reconnecting to get a place back ----
    if (resume) {
      const [id, secret] = resume.split(".");
      const rec = id && (await this.ctx.storage.get(`m:${id}`));
      if (!rec || rec.secret !== secret) {
        this.send(server, { type: "resume_failed" });
        server.close(1000, "resume failed");
        return answer;
      }
      // The room may still think the old connection is open: replace it quietly.
      for (const m of this.members()) {
        if (m.info.id === id) {
          m.ws.serializeAttachment(null);
          try {
            m.ws.close(4000, "replaced");
          } catch {}
        }
      }
      rec.goneSince = null;
      await this.ctx.storage.put(`m:${id}`, rec);
      server.serializeAttachment({ id, name: rec.name, approved: true, host: rec.host, hb, since: Date.now() });
      this.send(server, { type: "resumed", host: rec.host });
      for (const message of (await this.ctx.storage.get(`q:${id}`)) || []) this.send(server, message);
      await this.ctx.storage.delete(`q:${id}`);
      if (rec.host) await this.sendWaitingTo(id);
      await this.scheduleAlarm();
      return answer;
    }

    // ---- joining ----
    const records = await this.records();
    const isHost = url.searchParams.get("token") === room.hostToken && !records.some((r) => r.host);
    const id = `m${room.nextId}`;
    const order = room.nextId;
    room.nextId += 1;
    await this.ctx.storage.put("room", room);
    server.serializeAttachment({ id, name, approved: isHost, host: isHost, hb, since: Date.now() });

    if (isHost) {
      await this.admit(server, { id, name, order, host: true }, room);
    } else if (records.length >= room.settings.max_people) {
      this.send(server, { type: "room_full" });
      server.close(1000, "room full");
    } else {
      this.send(server, { type: "waiting" });
      const host = records.find((r) => r.host);
      if (host) await this.deliver(host.id, { type: "join_request", id, name });
    }
    return answer;
  }

  /// Let someone in: store them, welcome them, and tell everyone else.
  async admit(ws, { id, name, order, host }, room) {
    const others = await this.records();
    const secret = crypto.randomUUID();
    await this.ctx.storage.put(`m:${id}`, { id, name, secret, host, order, goneSince: null });
    if (room.emptySince) {
      room.emptySince = null;
      await this.ctx.storage.put("room", room);
    }
    this.send(ws, {
      type: "welcome",
      you: id,
      host,
      members: others.map((r) => ({ id: r.id, name: r.name })),
      settings: room.settings,
      resume: secret,
    });
    for (const r of others) await this.deliver(r.id, { type: "member_joined", id, name });
    await this.scheduleAlarm();
  }

  // ---------- messages ----------

  async webSocketMessage(ws, raw) {
    const me = ws.deserializeAttachment();
    if (!me || typeof raw !== "string" || raw.length > MAX_SIGNAL_BYTES * 2) return;
    let msg;
    try {
      msg = JSON.parse(raw);
    } catch {
      return;
    }

    if ((msg.type === "approve" || msg.type === "decline") && me.host) {
      const target = this.members().find((m) => !m.info.approved && m.info.id === msg.id);
      if (!target) return;
      if (msg.type === "decline") {
        this.send(target.ws, { type: "declined" });
        target.ws.close(1000, "declined");
        return;
      }
      const room = await this.ctx.storage.get("room");
      if ((await this.records()).length >= room.settings.max_people) {
        this.send(target.ws, { type: "room_full" });
        target.ws.close(1000, "room full");
        return;
      }
      target.info.approved = true;
      target.ws.serializeAttachment(target.info);
      const order = Number(target.info.id.slice(1));
      await this.admit(target.ws, { id: target.info.id, name: target.info.name, order, host: false }, room);
      return;
    }

    // The host removes someone: tell them, close their connection (no reconnecting:
    // their place is deleted), and tell everyone else at once.
    if (msg.type === "remove" && me.host && typeof msg.id === "string" && msg.id !== me.id) {
      if (!(await this.ctx.storage.get(`m:${msg.id}`))) return;
      const target = this.live(msg.id);
      if (target) {
        target.ws.serializeAttachment(null);
        this.send(target.ws, { type: "removed" });
        try {
          target.ws.close(4002, "removed by the host");
        } catch {}
      }
      await this.leave(msg.id);
      return;
    }

    // The host changes the receive buffer: store it and tell everyone (the host too,
    // as confirmation). Newcomers get it in "welcome".
    if (msg.type === "set_buffer" && me.host) {
      const room = await this.ctx.storage.get("room");
      const buffer_ms = clampSetting(msg.buffer_ms, BUFFER_LIMITS);
      if (buffer_ms === room.settings.buffer_ms) return;
      room.settings = { ...room.settings, buffer_ms };
      await this.ctx.storage.put("room", room);
      for (const r of await this.records()) await this.deliver(r.id, { type: "settings", settings: room.settings });
      return;
    }

    if (msg.type === "signal" && me.approved && typeof msg.data === "string" && msg.data.length <= MAX_SIGNAL_BYTES) {
      if (typeof msg.to === "string" && (await this.ctx.storage.get(`m:${msg.to}`))) {
        await this.deliver(msg.to, { type: "signal", from: me.id, data: msg.data });
      }
    }
  }

  // ---------- leaving ----------

  async webSocketClose(ws, code) {
    await this.closed(ws, code === 1000 || code === 1005);
  }

  async webSocketError(ws) {
    await this.closed(ws, false);
  }

  /// A connection ended. `onPurpose` = the member said goodbye (left the collab);
  /// otherwise it dropped, and they get GRACE_MS to reconnect.
  async closed(ws, onPurpose) {
    const me = ws.deserializeAttachment();
    if (!me) return;
    ws.serializeAttachment(null); // don't handle the same departure twice
    if (!me.approved) return this.cancelWaiting(me.id);
    if (onPurpose) await this.leave(me.id);
    else await this.dropped(me.id);
  }

  /// Member `id` is gone for good: tell the others, and pass on the host role.
  async leave(id) {
    const rec = await this.ctx.storage.get(`m:${id}`);
    if (!rec) return;
    await this.ctx.storage.delete([`m:${id}`, `q:${id}`]);
    const others = await this.records();
    for (const r of others) await this.deliver(r.id, { type: "member_left", id });

    // The host left: the longest-standing member becomes host (preferring someone
    // who's connected right now), and gets any waiting requests.
    if (rec.host && others.length) {
      others.sort((a, b) => !!a.goneSince - !!b.goneSince || a.order - b.order);
      const next = others[0];
      next.host = true;
      await this.ctx.storage.put(`m:${next.id}`, next);
      const m = this.live(next.id);
      if (m) {
        m.info.host = true;
        m.ws.serializeAttachment(m.info);
      }
      await this.deliver(next.id, { type: "you_are_host" });
      await this.sendWaitingTo(next.id);
    }

    // Nobody left? Delete the room after a while.
    if (!others.length) {
      const room = await this.ctx.storage.get("room");
      room.emptySince = Date.now();
      await this.ctx.storage.put("room", room);
    }
    await this.scheduleAlarm();
  }

  async alarm() {
    const room = await this.ctx.storage.get("room");
    if (!room) return;
    await this.sweep();
    const now = Date.now();
    const connected = new Set(this.members().filter((m) => m.info.approved).map((m) => m.info.id));
    for (const r of await this.records()) {
      if (r.goneSince && now - r.goneSince >= GRACE_MS) await this.leave(r.id);
      // Stored but not connected and not marked (e.g. the room restarted): start their grace now.
      else if (!r.goneSince && !connected.has(r.id)) await this.dropped(r.id);
    }

    const fresh = await this.ctx.storage.get("room");
    if (fresh.emptySince && now >= fresh.emptySince + this.emptyLifetimeMs() && !(await this.records()).length) {
      for (const m of this.members()) m.ws.close(1000, "room closed");
      await this.ctx.storage.deleteAll();
      return;
    }
    await this.scheduleAlarm();
  }
}
