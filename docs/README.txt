VTube Collab {VERSION}

Collab with other VTubers using any VTubing app. Everyone's avatar travels directly
between your PCs, transparent background included, and shows up in OBS as its own
source.

You need: Windows 10/11, OBS with the Spout2 plugin, and your VTubing app with Spout2
output turned on.


INSTALL
1. Unzip the whole folder somewhere (e.g. Documents). Keep the files together.
2. Double-click "Start VTube Collab.bat".

"Windows protected your PC"? Click "More info", then "Run anyway". Windows shows this
for programs that aren't signed with a paid certificate; it doesn't mean anything was
found. You only need to do it once per version.

Windows Firewall asks? Click "Allow". Without it, partners can't connect to you.


START A COLLAB
1. Enter your name and pick your avatar (your VTubing app's Spout2 output).
2. Host: "Host a collab..." > choose the settings > "Open room". Share the room code
   (like MINTY-OTTER-42) and click "Accept" for each person who asks to join.
   Everyone else: "Join a collab..." > type the code.
3. In OBS, add a "Spout2 Capture" source "Collab - <name>" for each person.
   Or tick "Publish a group source" and add "Collab - Group" once for everyone.

The host can remove people and change the receive buffer at any time. If the host
leaves, the next person becomes host.


ROOM SETTINGS
- Max people (you included). Bigger rooms send smaller video: 1080p up to 4 people,
  720p up to 8, 540p above that. Each person needs about 25 Mbit/s upload for a full
  room of 8 (check at speedtest.net).
- Receive buffer: how long incoming video is held before it's shown. More is smoother
  on bad connections, but avatars appear a little later. 120 ms suits most people. If
  someone's avatar stutters ("frames skipped" in the People list), try 200-300 ms.


IF SOMETHING GOES WRONG
"Room codes are unavailable"
  The room server is free to use and has a daily allowance. When it's used up, rooms
  can't be opened or joined until midnight UTC (the app says how long). Collabs that
  are already running keep going. Meanwhile you can collab with one person using long
  codes: the app shows an invite code to send them, they paste it into "Join a
  collab...", and send you back their reply code.
  If it says the server can't be reached: check your internet, and that no firewall
  or antivirus is blocking VTube Collab.

"Couldn't connect" next to someone
  There's no direct route between your two PCs. Check that VTube Collab is allowed
  through Windows Firewall on both PCs. Some work, school or hotel networks and mobile
  hotspots block direct connections; there's no way around that yet.

Your avatar isn't in the list
  Turn on Spout2 output in your VTubing app, then wait a couple of seconds.

The video gets a bit softer
  When your partners lose part of your video (usually because your upload is busy),
  VTube Collab lowers your quality a little, and raises it again once things are
  stable.

Laptop with two graphics cards
  If your avatar can't be opened, set your VTubing app, OBS and VTube Collab to "High
  performance" in Windows Settings > System > Display > Graphics, then restart them.


REPORTING A PROBLEM
Open an issue at https://github.com/WindowsBen/VTube-Collab/issues. Say what happened,
who looked bad to whom, roughly when, and attach your log: File > Open log folder >
the newest .log file. Logs contain technical details and the names of the people in
the room, nothing else.


PRIVACY
- Video goes directly between your PCs, encrypted. Nobody else gets it.
- The room server (on Cloudflare) passes connection details between the people in a
  room. It sees your name, the room code and your IP address. The people in your room
  see your name and IP address (needed to connect directly). The server keeps nothing
  about you after you leave; an empty room is deleted after 10 minutes.
- At start, the app asks GitHub whether there's a newer version. Nothing about you is
  sent; GitHub sees your IP address, as any website does.
- Logs stay on your PC unless you send them to someone.


Updates: the app tells you when a new version is out (Help > About > Website).

VTube Collab is free and will stay free. If you'd like to say thanks, you can leave a
tip: https://streamelements.com/windowsben/tip (also under Help > Leave a tip...).

VTube Collab is open source under the MIT licence. It includes GStreamer (LGPL) and
other open-source libraries, plus the Microsoft C++ runtime. All licences are in the
"licenses" folder.
