# Project VTube Collab — Design Document

_Last updated: 2026-11-16_

## 1. Goal

Let VTubers using **different** VTubing programs (VTube Studio, Warudo, VSeeFace, VNyan,
PNGtuber tools, etc.) collab with each other easily: low latency, correct resolution,
real transparency (no green-screen keying), and each partner showing up as a ready-made
source in OBS.

## 2. Decisions so far

| Topic | Decision |
|---|---|
| Language | Rust |
| UI | Native, not Electron. Candidates: **Slint** or **egui** (to be decided) |
| Platform | **Windows first**. macOS (Syphon) maybe later |
| Room size | Flexible: 2 to 20 people |
| Audio | **Not carried.** Voice stays in Discord etc. Per-partner **delay slider** for lip-sync |
| Networking | Peer-to-peer (WebRTC-style direct connections) |
| Handshake | Small handshake-only server on a **free tier** (Cloudflare Workers), with **invite codes** as a no-server backup |
| Video pipeline | **GStreamer** (hardware encoding for NVIDIA/AMD/Intel, built-in WebRTC), behind our own interfaces |
| Relay fallback | Allowed for users whose networks block direct connections, as long as it costs nothing (check Cloudflare's relay free allowance before relying on it) |

## 3. How it works (high level)

```
 Your VTubing app ──Spout2/NDI/capture──► VTube Collab ──P2P video──► Partner's VTube Collab ──Spout2──► Partner's OBS
```

### 4.1 Input: capture the output, don't integrate with every app
- **Spout2** (primary): shares the rendered image straight from the graphics card, transparency included.
- **NDI** (secondary).
- **Virtual webcam / window capture** (fallback, e.g. browser-based PNGtuber tools).
- "Setup detection" = list available Spout/NDI sources, optionally hint based on
  running programs. The user always confirms.

### 4.2 Transport: transparent video over P2P
- Common hardware encoders (NVENC / AMF / QuickSync, H.264/HEVC) don't carry transparency.
- Approach: send the **colour image + transparency mask side by side in one double-size frame**,
  recombine on the receiver. No green-screen keying needed.
- Room topology:
  1. **Phase 1:** everyone connects directly to everyone (full mesh), with automatic quality
     limits as the room grows. Comfortable up to ~6 people on typical home upload speeds.
  2. **Later:** for big rooms, a participant with strong upload can volunteer as a **hub**
     that forwards streams. Code is structured so this can be added without a rewrite.

Rough upload need per person (~3 Mbps per 720p30 stream with transparency):

| Room size | Streams uploaded per person | Upload |
|---|---|---|
| 2 | 1 | ~3 Mbps |
| 4 | 3 | ~9 Mbps |
| 8 | 7 | ~21 Mbps |
| 20 | 19 | ~57 Mbps |

### 4.3 Output: partners appear in OBS
- **Phase 1:** each partner is published as its own **Spout2 source**. OBS picks it up with the Spout2 plugin.
- **Later:** possibly a native OBS plugin for smoother setup.

### 4.4 Dev machine notes
- GStreamer 1.28.8 installed per-user at `%LOCALAPPDATA%\Programs\gstreamer\1.0\msvc_x86_64`
  (`bin` on PATH, `PKG_CONFIG_PATH` → `lib\pkgconfig`).
- Available: `webrtcbin`, NVIDIA (`nvh264enc`, `nvd3d11h264enc`, `nvh265enc`), AMD (`amfh264enc`),
  Media Foundation (`mfh264enc`), D3D11 upload/convert/decode. No Intel QSV (no Intel GPU).
- **Two GPUs** (AMD integrated + NVIDIA RTX 3060). A Spout2 shared texture can only be opened on
  the GPU the sender renders on. The default adapter happened to match, but the app must pick the
  adapter explicitly (match the sender's GPU) on multi-GPU PCs.

## 4. Ideas parked for later
- Sending **tracking data only** (VMC protocol) for 3D users: tiny bandwidth, but the partner
  needs the model file (licensing concerns).
- Native OBS plugin.
- macOS support.
- Volunteer hub for large rooms.

## 5. Known issues (may revisit)
- **Host received only ~50–55 fps** in the 2-PC test (2026-11-17), while the friend sent and received 60.
  Decoder confirmed on the RTX 3060 (not the AMD iGPU). Likely local load on the host PC, or packet loss
  friend → host. Since then: NACK enabled, 120 ms buffer default, and per-partner loss diagnostics — the next
  real test will show whether it's loss.
- **Round-trip time in the diagnostics reads ~1 ms in local simulations** (netsim only delays media, not RTCP);
  real connections should show real values — check in the next real test.
- **Received fps fluctuates between 5 s windows** (e.g. 50 then 69) because frames arrive in bursts; the average is right.
- **Resource use of the app** (measured 2026-11-17, two instances on one PC): idle < 0.2% CPU and ~200 MB RAM
  each; connected ~17.5% of total CPU (~2.1 cores) and ~500 MB each. Idle memory is mostly the OpenGL context /
  graphics driver; connected adds GStreamer, encoder/decoder and frame buffers. Possible later: zero-copy GPU path
  (CPU), smaller buffers / wgpu renderer (memory).

## 6. Open questions
- UI library: Slint or egui?
- ~~Video pipeline~~ **Decided (2026-11-16): GStreamer 1.28.x** for encode/decode and WebRTC.
  Kept behind our own small "encoder" and "connection" interfaces, so parts can later be swapped
  for FFmpeg + `str0m` if app size or overhead becomes a problem. Adds ~60–100 MB of bundled DLLs.
- Exact P2P library: `webrtc-rs`, `str0m`, or `iroh`.

## 7. Roadmap

### Milestone 1 — proof of concept
Two people, Windows only: invite code → one Spout2 source in → transparent video over a direct
connection → one Spout2 source out into OBS. Proves latency and transparency.

Progress:
- ✅ Grab one frame from a Spout2 sender with transparency (`cargo run -- grab "<name>"`).
  Verified with Avatar Live. Note: Avatar Live leaves a faint alpha haze (1–15) over ~34% of the frame,
  likely from its post-processing; check whether disabling bloom/effects removes it.
- ✅ Continuous capture at 60 fps (`cargo run --release -- watch "<name>" [seconds]`).
  1920×1080 from Avatar Live: avg 2.24 ms per grab (99% under 3.68 ms) vs 16.67 ms budget, 0 late frames.
- ✅ Local encode/decode loopback (`encode-test/`): colour + alpha packed side by side (3840×1080),
  `nvh264enc` (p1, ultra-low-latency, CBR) → `nvh264dec`. At 8 Mbit/s: capture→decoded avg 17.4 ms
  (99% under 23.8 ms), ~25 ms incl. capture + packing. Alpha avg error 0.4/255, no new haze.
  Colour PSNR 33.7 dB (34.9 dB at 16 Mbit/s) — limited by 4:2:0 chroma, not bitrate.
  Later: pack/convert on the GPU (saves ~8–10 ms); consider 4:4:4 or HEVC for sharper colour.
- ✅ Spout2 output: decoded frames published as Spout2 sender "VTube Collab test" (`spout-probe/src/sender.rs`),
  verified in OBS (Spout2 Capture) with correct transparency. Removed from Spout's list on exit.
  Known: solid alpha comes back as 250–254 (YUV rounding); not visible. Optional fix: snap ≥250 → 255, ≤4 → 0.
- ✅ One-way P2P on one PC (`collab host "<sender>"` / `collab join`): copy-paste invite/reply codes
  (~1000 chars, deflate + base64, prefix `VTC1-`), `webrtcbin` with Google STUN, jitter buffer 50 ms.
  60 fps sent and received; guest publishes Spout2 "Collab - <name>" with transparency. Clean exit on Enter.
- ✅ Two-way P2P on one PC (`collab host|join "<sender>" [--name X]`): both send and receive,
  Spout2 "Collab - <partner>" on each side, correct transparency. Encoder fallback: NVIDIA → AMD → Media Foundation.
  **Problem:** ~3 CPU cores per copy (CPU pack, `videoconvert`, CPU unpack); two copies on one PC reach only
  ~46 fps sent / ~35 received. Tried `d3d11convert` on receive: shifted alpha levels and was slower — reverted.
- ✅ Cut CPU use: GPU shaders (`vtc-core/src/shaders.hlsl`, `gpu.rs`) pack the avatar to NV12 (BT.709 limited)
  and unpack straight into the Spout texture. Two-way on one PC: 60 fps both ways, ~150% of one core per copy
  (was ~300%), whole PC 50% (was 84%). Solid alpha now exactly 255; alpha ≤2 snapped to 0.
  Not yet < 1 core: remaining cost is CPU↔GPU copies (readback for encoder, decoder download, upload).
  Later: zero-copy via GStreamer D3D12 (`gstreamer-d3d12` bindings exist) or double-buffered readback.
- ✅ Test bundle (`tools/make-bundle.ps1` → `dist/VTubeCollab-test.zip`, 12.2 MB): collab.exe + 45 GStreamer DLLs
  (found via `dumpbin /dependents`) + 15 plugins + MSVC runtime + licences. Verified with GStreamer hidden from
  PATH: all plugins loaded from the bundle. Friends must "Unblock" the zip; unsigned exe → SmartScreen warning.
- ✅ Two-PC test over the internet (2026-11-17): host (Avatar Live, NVIDIA) ↔ guest (Warudo), both saw each
  other's Spout source in OBS. Latency not measured yet.
- Usability notes from the test: users need to know their exact Spout sender name (consider `collab list`);
  README placeholders `<...>` were typed literally.

### Milestone 2 — usable by non-technical VTubers
- ✅ App window (`app/` → `VTubeCollab.exe`): egui/eframe 0.37, hand-drawn **XP Luna Blue** look
  (own title bar, rounded top corners, bevel buttons, group boxes, status bar, Tahoma from the user's
  Windows fonts — not shipped). Screens: Setup (name + avatar dropdown of Spout sources) → Host / Join
  (codes with Copy buttons) → Collab (status dot, OBS source name, fps). Connection runs on a worker thread.
- ✅ `collab` split into a library (`Connection` API + events) and the CLI; new `collab list`.
- ✅ Bundle includes the app + `Start VTube Collab.bat` (17.9 MB zip).
- ✅ Short room codes (e.g. `MINTY-OTTER-42`) via the handshake server in `handshake/` (this first "v1"
  mailbox design was later replaced by v2 rooms and removed — kept here as history):
  Cloudflare Worker + SQLite Durable Object per room, deployed 2026-11-17 to
  **https://vtube-collab.mpavatars-relay.workers.dev** (owner's existing account, **Workers Free plan**:
  over the shared 100k requests/day the server just errors until 00:00 UTC — never add a payment method or
  upgrade). Mailbox only: host posts invite → guest takes it once → guest posts reply → host polls (1/s) and
  takes it; room deleted. Rooms expire after 10 min (app offers "Get new room code"); codes ≤ 8 KB.
  The collab itself doesn't depend on the server after connecting. Long codes remain as fallback
  (app: "Use long codes instead"; CLI: `--long`). `VTC_SERVER` env var overrides the server (local tests:
  `npx wrangler dev`, `--var ROOM_LIFETIME_SECONDS:5` to test expiry).
  Redeploy: `cd handshake && npx wrangler deploy` (wrangler logged in via OAuth on the dev PC).
- ✅ Group source "Collab - Group" (`vtc-core/src/compositor.rs`, `collab/src/group.rs`): 1920×1080, every
  layer drawn full-canvas (aspect-fit, bottom-aligned) and stacked — positions come from each person's own
  VTubing app framing. GPU compositing in premultiplied alpha (16-bit float accumulation), resolved to
  straight alpha for OBS. Layers panel on the Collab screen: top = in front, checkbox = show/hide (your own
  row = "include me"), ▲/▼ to reorder; each person controls their own group source. Second copy on one PC
  publishes "Collab - Group 2". No measurable extra CPU.

### Milestone 3 — rooms for 2–100 people
- ✅ Room server v2 (`handshake/src/room_v2.js`, deployed alongside v1): one WebSocket per member to a
  hibernating Durable Object. Host approves each joiner (Accept/Decline); newcomer gets "welcome" + member
  list, others get "member_joined"; server only relays connection codes ("signals"). Host role passes to the
  longest-standing member if the host leaves. Empty room deleted after 10 min. Free plan: hibernation = no
  duration cost, incoming WS messages count 20:1, pings free.
- ✅ Multi-partner session (`collab/src/session.rs`, `room.rs`, `signal.rs`): encode once → `tee` → per-partner
  queue → RTP → own `webrtcbin`; per-partner decoder + Spout source + group layer. Newcomer makes offers to
  everyone already there. Bitrate per partner = 24 Mbit/s upload budget ÷ partners, clamped 2.5–8 Mbit/s.
- ✅ Host room settings (sent in the create request, enforced by the server, delivered in "welcome"):
  **max people 2–100 (default 20, counts the host)**, **receive buffer 20–500 ms (default 120)** applied by everyone.
- ✅ Loss handling: NACK on every transceiver; receiver `rtph264depay request-keyframe=true wait-for-keyframe=true`
  (damaged frames dropped + fresh keyframe requested instead of smearing).
  Background: 3-person test (2026-11-17, Canada) — one friend saw another's avatar smear while moving; link-specific
  packet loss with a 50 ms buffer too short for resends. Reproduced on one PC with `VTC_SIMULATE_LOSS=0.03`
  `VTC_SIMULATE_DELAY_MS=60` (test-only `netsim` via webrtcbin "request-aux-sender"):
  50 ms buffer → 16–26 fps, ~110 unrepaired losses/5 s; **120 ms → 50–59 fps, 1–12 unrepaired, clean picture**.
- ✅ Diagnostics per partner: packet loss % (raw, from nested `gst-rtpsource-stats`), "unrepaired" (FIR+PLI
  count = frames skipped), round-trip time. App shows "N skipped (try a bigger buffer)" in red when frequent.
- Not verifiable on one PC: 3+ people at full rate (3 copies = 3 encodes + 6 decodes on one GPU → ~15 fps).
  2 people in a v2 room: ~57–60 fps. Needs a real multi-PC test; receive path CPU per partner will matter for
  big rooms (→ zero-copy decode/unpack on the GPU).

- ✅ XP redesign of the app (2026-11-17): window auto-sizes to each screen (max 860 px, then scrolls);
  Trebuchet MS bold title, glossy caption buttons (maximise greyed), dark-edged blue frame; File/Help menu
  bar; XP dropdown arrow button, green-tick checkboxes, trackbar sliders, 75×23 buttons right-aligned at the
  bottom; status bar with sunken panels + grip; 16×16/32×32 app icon (title bar, taskbar).
  About and How-to are separate, movable OS windows (egui immediate viewports) opened beside the main window,
  so the main window stays usable. Test-only `VTC_PREVIEW=<screen>[+about|+howto]` shows any screen with
  sample data, for screenshots.
- ✅ Group source on/off switch (default **off**): off = the group thread is stopped and "Collab - Group" is
  removed from OBS's list. App: checkbox in the "Group source" box; CLI: `--group`.
- Clean-up (2026-11-17): removed `encode-test/` (early CPU loopback tool), `vtc-core/src/packing.rs` (CPU
  packing, replaced by the GPU shaders), the CPU upload in the Spout sender, and **room server v1** (one-time
  mailbox; deleted with migration `v3`). Old test bundles from before rooms v2 now fall back to long codes.
- ✅ **Beta 0.2.0** (2026-11-17), for an 8-person test group:
  - Log file per run in `%LOCALAPPDATA%\VTubeCollab\logs\` (newest 10 kept): version, Windows, graphics
    cards, Spout sources, all events and stats, crashes with backtrace. Never the long codes.
    App: File → Open log folder.
  - Remembers name, avatar, room settings and the group-source switch (`%APPDATA%\VTubeCollab\settings.json`).
  - Video size per person by room size: up to 4 → 1920×1080, 5–8 → 1280×720, more → 960×540 (bitrate
    ranges scaled to match). Joiners switch to the host's size on Welcome.
  - Any avatar size/aspect: GPU shader scales it into the transmit size (bilinear, premultiplied, centred,
    bottom-aligned); resizing the avatar mid-session reopens the packer instead of erroring.
  - Encoder paths verified at 60 fps with a correct picture: NVIDIA, AMD (`amfh264enc`), Windows
    (`mfh264enc`); AMD-style decoder via test-only `VTC_DECODER`. Intel Quick Sync not testable here.
  - One short readme + tester guide `docs/README.txt` (bundled as README.txt); zip named `VTubeCollab-<version>.zip`.

Code layout: `vtc-core/` (shared: Spout capture/output, GPU pack/unpack, compositor), `collab/` (connection
engine + CLI), `app/` (window), `handshake/` (Cloudflare room server), `spout-probe/` (test tool: list/grab/watch).

### Setup steps (in order)
1. ✅ Write this design document
2. ✅ Install the Rust toolchain (`rustup`) — already present: Rust 1.97.1 (msvc) + VS Build Tools 2026
3. ✅ Set up a local git repository
4. ✅ First program: list the Spout2 sources on this PC (`spout-probe/`) — verified with Avatar Live (1920×1080, RGBA 8-bit)
