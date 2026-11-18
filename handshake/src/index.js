// VTube Collab room server, on Cloudflare Workers.
//
// It lets people find each other with a short room code like MINTY-OTTER-42 and
// passes connection codes between them. It never sees video. The rooms themselves
// are in room_v2.js.
//
//   POST /v2/rooms  body (optional): {max_people, buffer_ms}
//                                          → 201 {code, token, settings}   (token = host's secret)
//   GET  /v2/rooms/<code>/ws?name=..&token=..   WebSocket into the room
//   429 = the free plan's daily allowance is used up (works again after midnight UTC)

import { roomSettings } from "./room_v2.js";
export { RoomV2 } from "./room_v2.js"; // rooms for 2–100 people

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

export default {
  async fetch(request, env) {
    try {
      return await handle(request, env);
    } catch (e) {
      // On the free plan, Durable Objects stop for the day once their daily allowance is
      // used up. Answer 429, like Workers' own daily limit, so the app can explain it.
      if (/exceeded|free tier|limit/i.test(String(e))) return text("daily limit reached", 429);
      throw e;
    }
  },
};

async function handle(request, env) {
  const url = new URL(request.url);
  const parts = url.pathname.split("/").filter(Boolean); // e.g. ["v2", "rooms", "MINTY-OTTER-42", "ws"]

  if (parts[0] !== "v2" || parts[1] !== "rooms") {
    return text("VTube Collab room server", 200);
  }
  const room = (code) => env.ROOMS2.get(env.ROOMS2.idFromName(code));

  // Create a room with the host's settings.
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
      const token = await room(code).create(settings);
      if (token) {
        return new Response(JSON.stringify({ code, token, settings }), {
          status: 201,
          headers: { "content-type": "application/json" },
        });
      }
    }
    return text("couldn't find a free room code, try again", 503);
  }

  // Join a room (WebSocket).
  if (parts.length === 4 && parts[3] === "ws") {
    const code = normaliseRoomCode(parts[2]);
    if (!code) return text("not a room code", 404);
    return room(code).fetch(request);
  }
  return text("not found", 404);
}
