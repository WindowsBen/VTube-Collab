// VTube Collab handshake server — a tiny "mailbox" on Cloudflare Workers.
//
// It only passes the two connection codes between host and guest, so people can
// share a short room code like MINTY-OTTER-42 instead of ~1000-character codes.
// It never sees video, and forgets everything after use (or after 10 minutes).
//
//   POST /rooms                 body: host's invite code   → 201, body: room code
//   GET  /rooms/<code>/invite   guest takes the invite (once) → 200 invite | 404 | 409
//   PUT  /rooms/<code>/reply    body: guest's reply code   → 200 | 404 | 409
//   GET  /rooms/<code>/reply    host checks for the reply  → 200 reply | 204 not yet | 404
//
// Each room is a "Durable Object": a tiny piece of storage that always gives the
// same answer to everyone, wherever in the world they connect from.

import { DurableObject } from "cloudflare:workers";
export { RoomV2 } from "./room_v2.js"; // v2: rooms for 2–100 people (see room_v2.js)
import { roomSettings } from "./room_v2.js";

// A room waits 10 minutes for a guest. (Setting ROOM_LIFETIME_SECONDS overrides
// this, e.g. `wrangler dev --var ROOM_LIFETIME_SECONDS:5` to test expiry quickly.)
const DEFAULT_ROOM_LIFETIME_SECONDS = 10 * 60;
const MAX_CODE_BYTES = 8 * 1024; // connection codes are ~1 KB; refuse anything big
const CODE_PREFIX = "VTC1-"; // every VTube Collab connection code starts with this

// Room codes: WORD-WORD-NN (about 1.6 million combinations).
const WORDS = [
  "AMBER", "APPLE", "ASTRO", "BADGE", "BAGEL", "BASIL", "BEACH", "BERRY", "BIRCH", "BLAZE",
  "BLOOM", "BOOST", "BRAVE", "BRICK", "BUNNY", "CABIN", "CANDY", "CEDAR", "CHALK", "CHARM",
  "CHESS", "CHILI", "CIDER", "CLOUD", "CLOVER", "COCOA", "CORAL", "COMET", "CRANE", "CRISP",
  "DAISY", "DELTA", "DINGO", "DREAM", "EAGLE", "EMBER", "FABLE", "FAIRY", "FERRY", "FIZZY",
  "FLAME", "FLINT", "FOCUS", "FROST", "FUDGE", "GECKO", "GHOST", "GIANT", "GLEAM", "GLOW",
  "GRAPE", "GUMMY", "HAPPY", "HAZEL", "HONEY", "HYDRA", "IGLOO", "IVORY", "JELLY", "JOLLY",
  "KAZOO", "KIWI", "KOALA", "LASER", "LEMON", "LILAC", "LLAMA", "LOTUS", "LUCKY", "LUNAR",
  "MAGIC", "MANGO", "MAPLE", "MARSH", "MELON", "MERRY", "MINTY", "MOCHA", "MOOSE", "NOVA",
  "OASIS", "OCEAN", "OLIVE", "ONYX", "OPAL", "ORBIT", "OTTER", "PANDA", "PEACH", "PEARL",
  "PIXEL", "PLUM", "POLAR", "POPPY", "PRISM", "QUAIL", "QUILL", "RADAR", "RAVEN", "RIVER",
  "ROBIN", "ROCKET", "RUBY", "SAGE", "SALSA", "SHELL", "SHINY", "SLOTH", "SNOWY", "SOLAR",
  "SPARK", "SPICE", "STORM", "SUGAR", "SUNNY", "SWIFT", "TANGO", "TIGER", "TOAST", "TULIP",
  "TURBO", "VELVET", "VIOLA", "WAFFLE", "WHALE", "WIZARD", "ZEBRA", "ZESTY",
];

function newRoomCode() {
  const pick = () => WORDS[crypto.getRandomValues(new Uint32Array(1))[0] % WORDS.length];
  const number = crypto.getRandomValues(new Uint32Array(1))[0] % 100;
  return `${pick()}-${pick()}-${String(number).padStart(2, "0")}`;
}

/// Accept "minty otter 42", "Minty-Otter-42" etc. Returns null if it can't be a room code.
function normaliseRoomCode(text) {
  const parts = decodeURIComponent(text).toUpperCase().split(/[\s\-_]+/).filter(Boolean);
  if (parts.length !== 3 || !/^\d{1,2}$/.test(parts[2])) return null;
  return `${parts[0]}-${parts[1]}-${parts[2].padStart(2, "0")}`;
}

function text(body, status = 200) {
  return new Response(body, { status, headers: { "content-type": "text/plain; charset=utf-8" } });
}

/// Read a connection code from a request body, refusing anything that isn't one.
async function readCode(request) {
  const body = (await request.text()).trim();
  if (body.length > MAX_CODE_BYTES || !body.startsWith(CODE_PREFIX)) return null;
  return body;
}

// ---------------- one room ----------------

export class Room extends DurableObject {
  /// Load the room, or null if it doesn't exist or has expired.
  async load() {
    const room = await this.ctx.storage.get("room");
    if (!room) return null;
    if (Date.now() > room.expires) {
      await this.ctx.storage.deleteAll();
      return null;
    }
    return room;
  }

  /// Host creates the room. Returns false if this code is already in use.
  async create(invite) {
    if (await this.load()) return false;
    const seconds = Number(this.env.ROOM_LIFETIME_SECONDS) || DEFAULT_ROOM_LIFETIME_SECONDS;
    const expires = Date.now() + seconds * 1000;
    await this.ctx.storage.put("room", { invite, inviteTaken: false, reply: null, expires });
    await this.ctx.storage.setAlarm(expires); // tidy up even if nobody comes back
    return true;
  }

  /// Guest takes the invite. Only once, so nobody else can join the same room.
  async takeInvite() {
    const room = await this.load();
    if (!room) return { status: 404 };
    if (room.inviteTaken) return { status: 409 };
    room.inviteTaken = true;
    await this.ctx.storage.put("room", room);
    return { status: 200, body: room.invite };
  }

  async putReply(reply) {
    const room = await this.load();
    if (!room) return 404;
    if (!room.inviteTaken || room.reply) return 409;
    room.reply = reply;
    await this.ctx.storage.put("room", room);
    return 200;
  }

  /// Host checks for the reply. Once delivered, the room is deleted.
  async takeReply() {
    const room = await this.load();
    if (!room) return { status: 404 };
    if (!room.reply) return { status: 204 };
    await this.ctx.storage.deleteAll();
    return { status: 200, body: room.reply };
  }

  /// Runs when the room's lifetime is over: forget everything.
  async alarm() {
    await this.ctx.storage.deleteAll();
  }
}

// ---------------- routing ----------------

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const parts = url.pathname.split("/").filter(Boolean); // e.g. ["rooms", "MINTY-OTTER-42", "reply"]
    const room = (code) => env.ROOMS.get(env.ROOMS.idFromName(code));

    // ---- v2 (2–100 people) ----
    //   POST /v2/rooms  body (optional): {max_people, buffer_ms}
    //                                          → 201 {code, token, settings}   (token = host's secret)
    //   GET  /v2/rooms/<code>/ws?name=..&token=..   WebSocket into the room
    if (parts[0] === "v2" && parts[1] === "rooms") {
      const roomV2 = (code) => env.ROOMS2.get(env.ROOMS2.idFromName(code));
      if (parts.length === 2 && request.method === "POST") {
        let body = {};
        try {
          const raw = await request.text();
          if (raw.length > 0 && raw.length < 1024) body = JSON.parse(raw);
        } catch {
          // Not JSON: use the defaults.
        }
        const settings = roomSettings(body);
        for (let attempt = 0; attempt < 5; attempt++) {
          const code = newRoomCode();
          const token = await roomV2(code).create(settings);
          if (token) {
            return new Response(JSON.stringify({ code, token, settings }), {
              status: 201,
              headers: { "content-type": "application/json" },
            });
          }
        }
        return text("couldn't find a free room code, try again", 503);
      }
      if (parts.length === 4 && parts[3] === "ws") {
        const code = normaliseRoomCode(parts[2]);
        if (!code) return text("not a room code", 404);
        return roomV2(code).fetch(request);
      }
      return text("not found", 404);
    }

    if (parts[0] !== "rooms") {
      return text("VTube Collab handshake server", 200);
    }

    // POST /rooms — create a room for the host's invite code.
    if (parts.length === 1 && request.method === "POST") {
      const invite = await readCode(request);
      if (!invite) return text("not a VTube Collab code", 400);
      for (let attempt = 0; attempt < 5; attempt++) {
        const code = newRoomCode();
        if (await room(code).create(invite)) return text(code, 201);
      }
      return text("couldn't find a free room code, try again", 503);
    }

    if (parts.length !== 3) return text("not found", 404);
    const code = normaliseRoomCode(parts[1]);
    if (!code) return text("not a room code", 404);

    if (parts[2] === "invite" && request.method === "GET") {
      const r = await room(code).takeInvite();
      return text(r.body ?? (r.status === 409 ? "someone already joined this room" : "room not found or expired"), r.status);
    }
    if (parts[2] === "reply" && request.method === "PUT") {
      const reply = await readCode(request);
      if (!reply) return text("not a VTube Collab code", 400);
      const status = await room(code).putReply(reply);
      return text(status === 200 ? "ok" : status === 409 ? "reply not expected" : "room not found or expired", status);
    }
    if (parts[2] === "reply" && request.method === "GET") {
      const r = await room(code).takeReply();
      if (r.status === 204) return new Response(null, { status: 204 });
      return text(r.body ?? "room not found or expired", r.status);
    }
    return text("not found", 404);
  },
};
