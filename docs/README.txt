VTube Collab {VERSION} - thanks for testing!

You need: Windows 10/11, OBS with the Spout2 plugin, your VTubing app with Spout2 output on.

START
1. Double-click "Start VTube Collab.bat".
   "Windows protected your PC"? Click More info > Run anyway. Firewall asks? Click Allow.
2. Enter your name and pick your avatar.
3. Host: "Host a collab..." > Open room > share the code (e.g. MINTY-OTTER-42) > Accept people.
   Others: "Join a collab..." > type the code.
4. In OBS, add a Spout2 Capture source "Collab - <name>" for each person
   (or tick "Publish a group source" and add "Collab - Group" once).

PLEASE TRY
- Fill the room up to 8, one by one. Leave and rejoin. Let the host leave.
- Move a lot. Does anyone smear, stutter or freeze?
- Keep it running for 30+ minutes.

SOMETHING WRONG?
Tell us what happened, who looked bad to whom, and roughly when.
Send your log: File > Open log folder > newest .log file.
Stuttering? The host can set a bigger receive buffer (200-300 ms) next time.

KNOWN LIMITS
- Some networks (work, school, mobile hotspot) block direct connections. No fallback yet.
- No auto-reconnect: if your internet drops, rejoin.
- 5-8 people get 720p video, more than 8 get 540p.

Includes GStreamer (LGPL) and Microsoft C++ runtime libraries; licences in the 'licenses' folder.
