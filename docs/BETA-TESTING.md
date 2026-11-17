# VTube Collab 0.2.0-beta — tester guide

Thank you for testing! This is an early version: things **will** go wrong, and that's
exactly what we want to find. Please note what happened and send your log (see below).

## What you need

- Windows 10 or 11, any graphics card (NVIDIA, AMD or Intel).
- Your VTubing app with **Spout2 output turned on** (VTube Studio, Warudo, VSeeFace, VNyan, ...).
- **OBS** with the **Spout2 plugin** (to see the others).
- A wired connection if you can (Wi-Fi works, but is a common cause of stutter).

## Install

1. Unzip `VTubeCollab-0.2.0-beta.zip` anywhere (e.g. your Desktop).
2. Double-click **Start VTube Collab.bat**.
3. If Windows says **"Windows protected your PC"**: click **More info → Run anyway**
   (the app isn't signed yet).
4. If Windows Firewall asks: click **Allow**. Without this, others can't connect to you.

## How to use

1. Enter your name and pick your avatar (your VTubing app's Spout2 output).
2. **Host**: click *Host a collab...*, choose the settings, *Open room*, and share the room
   code (e.g. `MINTY-OTTER-42`). Click **Accept** when people ask to join.
3. **Everyone else**: click *Join a collab...*, type the room code, wait to be let in.
4. In OBS add a **Spout2 Capture** source for each person: `Collab - <their name>`.
   Or tick **Publish a group source** in the app and add `Collab - Group` for everyone at once.

*Help → How to use...* opens these steps in a separate window you can keep open.

## What to test (tick what you tried)

- [ ] **Two people** — host and join, both see each other in OBS, avatars transparent.
- [ ] **Growing the room** — join one by one up to 8 people. Does everyone see everyone?
- [ ] **Leaving and rejoining** — someone leaves and joins again with the same code.
- [ ] **Host leaves** — the next person becomes host and can accept newcomers.
- [ ] **Group source** — turn it on, add `Collab - Group` in OBS, reorder and hide layers.
- [ ] **Different VTubing apps** — note which app you use.
- [ ] **Movement** — move a lot: does anyone's avatar smear, stutter or freeze for others?
- [ ] **Receive buffer** — if someone stutters, the host can try a bigger buffer (200–300 ms)
      in the room settings next time.
- [ ] **Long session** — keep a collab running for 30+ minutes.

## What to report

Please send, for each problem:

1. **What you did** and **what happened** (screenshots help).
2. **Who** it affected (who saw whom badly) and **when** (rough time).
3. Your **log file**: in the app, *File → Open log folder*, and send the newest `.log` file
   (one is made per session). It contains your PC's graphics card(s), connection events and
   frame rates — no passwords, and not your connection codes.
4. What the **People** list showed for the affected person: fps, round trip, packet loss,
   and whether it said "frames skipped".

## Known limits in this beta

- **Strict networks**: some networks (some ISPs, mobile hotspots, university/work networks)
  block direct connections. That person will show "couldn't connect" for some or all others.
  There's no relay fallback yet.
- **No automatic reconnect**: if your internet drops, leave and rejoin the room.
- **Bigger rooms use smaller video**: 1080p up to 4 people, 720p up to 8, 540p above.
- **Unsigned app**: the "Windows protected your PC" warning is expected.
