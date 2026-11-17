// Room server v2: rooms for 2–20 people, joined with one room code.
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
//   {type:"signal", to, data}                          (approved members only)
// Room → client:
//   {type:"welcome", you, host, members:[{id,name}], settings:{max_people, buffer_ms}}
//                                                      you're in (members = who's already there)
//   {type:"waiting"}                                   waiting for the host to approve you
//   {type:"declined"} {type:"room_full"}               then the room closes your connection
//   {type:"join_request", id, name}                    (to host) someone wants to join
//   {type:"join_cancelled", id}                        (to host) they gave up waiting
//   {type:"member_joined", id, name}  {type:"member_left", id}
//   {type:"you_are_host"}                              the host left; you approve newcomers now
//   {type:"signal", from, data}
//
// Uses Cloudflare's WebSocket "hibernation": while nobody is talking the room sleeps
// and costs nothing. Who's who is stored on each connection ("attachment"), because
// the room's memory is wiped when it sleeps.

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
// An empty room (nobody connected) is deleted after 10 minutes.
const DEFAULT_EMPTY_LIFETIME_SECONDS = 10 * 60;

export class RoomV2 extends DurableObject {
  emptyLifetimeMs() {
    return (Number(this.env.ROOM_LIFETIME_SECONDS) || DEFAULT_EMPTY_LIFETIME_SECONDS) * 1000;
  }

  /// Host creates the room with `settings` (from roomSettings). Returns the host's
  /// secret token, or null if the code is taken.
  async create(settings) {
    if (await this.ctx.storage.get("room")) return null;
    const hostToken = crypto.randomUUID();
    await this.ctx.storage.put("room", { hostToken, nextId: 1, settings });
    await this.ctx.storage.setAlarm(Date.now() + this.emptyLifetimeMs());
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

  send(ws, message) {
    try {
      ws.send(JSON.stringify(message));
    } catch {
      // The connection is already closing; nothing to do.
    }
  }

  approvedList(except = null) {
    return this.members(except)
      .filter((m) => m.info.approved)
      .map((m) => ({ id: m.info.id, name: m.info.name }));
  }

  // ---------- new connections ----------

  async fetch(request) {
    const room = await this.ctx.storage.get("room");
    if (!room) return new Response("room not found or expired", { status: 404 });
    if (request.headers.get("Upgrade") !== "websocket") {
      return new Response("expected a WebSocket", { status: 426 });
    }

    const url = new URL(request.url);
    const name = (url.searchParams.get("name") || "Guest").trim().slice(0, MAX_NAME_LENGTH) || "Guest";
    const isHost = url.searchParams.get("token") === room.hostToken && !this.members().some((m) => m.info.host);

    const id = `m${room.nextId}`;
    room.nextId += 1;
    await this.ctx.storage.put("room", room);
    await this.ctx.storage.deleteAlarm(); // someone's here: the room stays

    const pair = new WebSocketPair();
    const [client, server] = Object.values(pair);
    this.ctx.acceptWebSocket(server, [id]);
    server.serializeAttachment({ id, name, approved: isHost, host: isHost });

    if (isHost) {
      this.send(server, { type: "welcome", you: id, host: true, members: this.approvedList(server), settings: room.settings });
    } else if (this.approvedList().length >= room.settings.max_people) {
      this.send(server, { type: "room_full" });
      server.close(1000, "room full");
    } else {
      this.send(server, { type: "waiting" });
      const host = this.members().find((m) => m.info.host);
      if (host) this.send(host.ws, { type: "join_request", id, name });
    }
    return new Response(null, { status: 101, webSocket: client });
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

    const find = (id) => this.members().find((m) => m.info.id === id);

    if ((msg.type === "approve" || msg.type === "decline") && me.host) {
      const target = find(msg.id);
      if (!target || target.info.approved) return;
      if (msg.type === "decline") {
        this.send(target.ws, { type: "declined" });
        target.ws.close(1000, "declined");
        return;
      }
      const room = await this.ctx.storage.get("room");
      if (this.approvedList().length >= room.settings.max_people) {
        this.send(target.ws, { type: "room_full" });
        target.ws.close(1000, "room full");
        return;
      }
      target.info.approved = true;
      target.ws.serializeAttachment(target.info);
      this.send(target.ws, {
        type: "welcome",
        you: target.info.id,
        host: false,
        members: this.approvedList(target.ws),
        settings: room.settings,
      });
      for (const m of this.members(target.ws)) {
        if (m.info.approved) this.send(m.ws, { type: "member_joined", id: target.info.id, name: target.info.name });
      }
      return;
    }

    if (msg.type === "signal" && me.approved && typeof msg.data === "string" && msg.data.length <= MAX_SIGNAL_BYTES) {
      const target = find(msg.to);
      if (target && target.info.approved) this.send(target.ws, { type: "signal", from: me.id, data: msg.data });
    }
  }

  // ---------- leaving ----------

  async webSocketClose(ws) {
    await this.left(ws);
  }

  async webSocketError(ws) {
    await this.left(ws);
  }

  async left(ws) {
    const me = ws.deserializeAttachment();
    if (!me) return;
    ws.serializeAttachment(null); // don't handle the same departure twice
    const others = this.members(ws);

    if (me.approved) {
      for (const m of others) if (m.info.approved) this.send(m.ws, { type: "member_left", id: me.id });
    } else {
      const host = others.find((m) => m.info.host);
      if (host) this.send(host.ws, { type: "join_cancelled", id: me.id });
    }

    // The host left: the longest-standing member becomes host, and gets any waiting requests.
    if (me.host) {
      const approved = others.filter((m) => m.info.approved);
      approved.sort((a, b) => Number(a.info.id.slice(1)) - Number(b.info.id.slice(1)));
      const next = approved[0];
      if (next) {
        next.info.host = true;
        next.ws.serializeAttachment(next.info);
        this.send(next.ws, { type: "you_are_host" });
        for (const m of others) {
          if (!m.info.approved) this.send(next.ws, { type: "join_request", id: m.info.id, name: m.info.name });
        }
      }
    }

    // Nobody left (except people still waiting)? Delete the room after a while.
    if (!others.some((m) => m.info.approved)) {
      await this.ctx.storage.setAlarm(Date.now() + this.emptyLifetimeMs());
    }
  }

  async alarm() {
    if (this.members().some((m) => m.info.approved)) return; // someone came back
    for (const m of this.members()) m.ws.close(1000, "room closed");
    await this.ctx.storage.deleteAll();
  }
}
