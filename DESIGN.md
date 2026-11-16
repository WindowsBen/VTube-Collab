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

## 5. Ideas parked for later
- Sending **tracking data only** (VMC protocol) for 3D users: tiny bandwidth, but the partner
  needs the model file (licensing concerns).
- Native OBS plugin.
- macOS support.
- Volunteer hub for large rooms.

## 6. Open questions
- UI library: Slint or egui?
- Video pipeline: GStreamer (faster to build, heavier) vs. custom (`str0m` + hardware encoders)?
  Leaning toward GStreamer for the first version.
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

### Setup steps (in order)
1. ✅ Write this design document
2. ✅ Install the Rust toolchain (`rustup`) — already present: Rust 1.97.1 (msvc) + VS Build Tools 2026
3. ✅ Set up a local git repository (never pushed by Claude)
4. ✅ First program: list the Spout2 sources on this PC (`spout-probe/`) — verified with Avatar Live (1920×1080, RGBA 8-bit)
