# Project VTube Collab — Design Document

_Last updated: 2026-11-16_

## 1. Goal

Let VTubers using **different** VTubing programs (VTube Studio, Warudo, VSeeFace, VNyan,
PNGtuber tools, etc.) collab with each other easily: low latency, correct resolution,
real transparency (no green-screen keying), and each partner showing up as a ready-made
source in OBS.

## 2. Ground rules for development

- Files are only created inside `E:\VTube Collab`.
- Nothing is pushed to GitHub by Claude — the project owner pushes.
- No change (files, commands, installs) is made without consulting the owner first.
- The owner is new to Rust: every step is explained in plain language.

## 3. Decisions so far

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

## 4. How it works (high level)

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

## 5. Ideas parked for later
- Sending **tracking data only** (VMC protocol) for 3D users: tiny bandwidth, but the partner
  needs the model file (licensing concerns).
- Native OBS plugin.
- macOS support.
- Volunteer hub for large rooms.

## 6. Known issues (may revisit)
- **Host received only ~50–55 fps** in the 2-PC test (2026-11-17), while the friend sent and received 60.
  Decoder confirmed on the RTX 3060 (not the AMD iGPU). Likely local load on the host PC, or packet loss
  friend → host. If revisited: add packet-loss / RTT / skipped-frame stats to `[stats]`, enable NACK retransmission.
- **Resource use of the app** (measured 2026-11-17, two instances on one PC): idle < 0.2% CPU and ~200 MB RAM
  each; connected ~17.5% of total CPU (~2.1 cores) and ~500 MB each. Idle memory is mostly the OpenGL context /
  graphics driver; connected adds GStreamer, encoder/decoder and frame buffers. Possible later: zero-copy GPU path
  (CPU), smaller buffers / wgpu renderer (memory).

## 7. Open questions
- UI library: Slint or egui?
- ~~Video pipeline~~ **Decided (2026-11-16): GStreamer 1.28.x** for encode/decode and WebRTC.
  Kept behind our own small "encoder" and "connection" interfaces, so parts can later be swapped
  for FFmpeg + `str0m` if app size or overhead becomes a problem. Adds ~60–100 MB of bundled DLLs.
- Exact P2P library: `webrtc-rs`, `str0m`, or `iroh`.

## 8. Roadmap

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
- ✅ Short room codes (e.g. `MINTY-OTTER-42`) via the handshake server in `handshake/`:
  Cloudflare Worker + SQLite Durable Object per room, deployed 2026-11-17 to
  **https://vtube-collab.mpavatars-relay.workers.dev** (owner's existing account, **Workers Free plan**:
  over the shared 100k requests/day the server just errors until 00:00 UTC — never add a payment method or
  upgrade). Mailbox only: host posts invite → guest takes it once → guest posts reply → host polls (1/s) and
  takes it; room deleted. Rooms expire after 10 min (app offers "Get new room code"); codes ≤ 8 KB.
  The collab itself doesn't depend on the server after connecting. Long codes remain as fallback
  (app: "Use long codes instead"; CLI: `--long`). `VTC_SERVER` env var overrides the server (local tests:
  `npx wrangler dev`, `--var ROOM_LIFETIME_SECONDS:5` to test expiry).
  Redeploy: `cd handshake && npx wrangler deploy` (wrangler logged in via OAuth on the dev PC).

Code layout: `vtc-core/` (shared: Spout capture/output, packing), `collab/` (the app),
`spout-probe/` and `encode-test/` (test tools).

### Setup steps (in order)
1. ✅ Write this design document
2. ✅ Install the Rust toolchain (`rustup`) — already present: Rust 1.97.1 (msvc) + VS Build Tools 2026
3. ✅ Set up a local git repository (never pushed by Claude)
4. ✅ First program: list the Spout2 sources on this PC (`spout-probe/`) — verified with Avatar Live (1920×1080, RGBA 8-bit)
