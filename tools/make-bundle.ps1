# Builds a self-contained VTube Collab folder + zip that runs WITHOUT GStreamer installed.
#
#   powershell -ExecutionPolicy Bypass -File tools\make-bundle.ps1
#
# Output: dist\VTubeCollab\  and  dist\VTubeCollab-<version>.zip  (version from app\Cargo.toml)
#
# Layout (GStreamer finds its plugins relative to its own DLLs, so this must be kept):
#   VTubeCollab\
#     Start VTube Collab.bat    double-click to open the app window
#     BETA-TESTING.txt          tester guide (from docs\BETA-TESTING.md)
#     bin\VTubeCollab.exe       the app window
#     bin\collab.exe            command-line tool (for testing)
#     bin\                      + GStreamer core DLLs + Microsoft C++ runtime DLLs
#     lib\gstreamer-1.0\        only the plugins collab uses
#     libexec\gstreamer-1.0\    gst-plugin-scanner.exe (GStreamer's plugin checker)
#     licenses\                 GStreamer's licence texts (required by the LGPL)
#     README.txt

$ErrorActionPreference = 'Stop'
$root = (Resolve-Path "$PSScriptRoot\..").Path
$dist = Join-Path $root 'dist'
$bundle = Join-Path $dist 'VTubeCollab'
$version = (Select-String -Path "$root\app\Cargo.toml" -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$zip = Join-Path $dist "VTubeCollab-$version.zip"

# ---- Where things are on the developer's PC ----
$gst = $env:GSTREAMER_1_0_ROOT_MSVC_X86_64
if (-not $gst) { $gst = "$env:LOCALAPPDATA\Programs\gstreamer\1.0\msvc_x86_64" }
if (-not (Test-Path "$gst\bin\gstreamer-1.0-0.dll")) { throw "GStreamer not found at $gst" }

$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$vsRoot = & $vswhere -latest -products * -property installationPath
$dumpbin = Get-ChildItem "$vsRoot\VC\Tools\MSVC\*\bin\Hostx64\x64\dumpbin.exe" | Sort-Object FullName | Select-Object -Last 1
$crt = Get-ChildItem "$vsRoot\VC\Redist\MSVC\*\x64\Microsoft.VC*.CRT" -Directory | Sort-Object FullName | Select-Object -Last 1
if (-not $dumpbin) { throw "dumpbin.exe not found (Visual Studio Build Tools)" }
if (-not $crt) { throw "Microsoft C++ runtime (VC\Redist) not found" }

# The plugins collab uses (see collab/src: session.rs, encoder.rs, and what webrtcbin needs inside).
$plugins = @(
    'gstapp',                # appsrc, appsink
    'gstcoreelements',       # capsfilter, queue, funnel
    'gstvideoparsersbad',    # h264parse
    'gstrtp',                # rtph264pay/depay, rtpstorage, FEC
    'gstrtpmanager',         # rtpbin, jitter buffer, rtpfunnel, retransmission
    'gstwebrtc',             # webrtcbin
    'gstnice',               # nicesrc/nicesink (connection setup through routers)
    'gstdtls',               # encryption handshake
    'gstsrtp',               # encrypted video packets
    'gstsctp',               # data channels (used by webrtcbin)
    'gstd3d11',              # d3d11h264dec (decoding on any GPU)
    'gstnvcodec',            # NVIDIA encoder
    'gstamfcodec',           # AMD encoder
    'gstmediafoundation',    # Windows encoder (fallback)
    'gstvideoconvertscale'   # videoconvert (used by the AMD / Windows encoder paths)
)

# ---- Build collab ----
Write-Host "Building VTubeCollab.exe and collab.exe (release)..."
Push-Location $root
cargo build --release -p collab -p app
if ($LASTEXITCODE -ne 0) { Pop-Location; throw "build failed" }
Pop-Location

# ---- Fresh output folder ----
if (Test-Path $bundle) { Remove-Item $bundle -Recurse -Force }
if (Test-Path $zip) { Remove-Item $zip -Force }
foreach ($d in 'bin', 'lib\gstreamer-1.0', 'libexec\gstreamer-1.0', 'licenses') {
    New-Item -ItemType Directory -Force (Join-Path $bundle $d) | Out-Null
}

Copy-Item "$root\target\release\VTubeCollab.exe" "$bundle\bin\"
Copy-Item "$root\target\release\collab.exe" "$bundle\bin\"
Copy-Item "$gst\libexec\gstreamer-1.0\gst-plugin-scanner.exe" "$bundle\libexec\gstreamer-1.0\"
foreach ($p in $plugins) { Copy-Item "$gst\lib\gstreamer-1.0\$p.dll" "$bundle\lib\gstreamer-1.0\" }

# ---- Copy every GStreamer DLL these files need, and what those need, and so on ----
# `dumpbin /dependents` lists the DLLs a file loads. DLLs not in GStreamer's bin folder
# are part of Windows (or the C++ runtime, handled below) and are skipped.
$queue = New-Object System.Collections.Queue
$queue.Enqueue("$bundle\bin\VTubeCollab.exe")
$queue.Enqueue("$bundle\bin\collab.exe")
$queue.Enqueue("$bundle\libexec\gstreamer-1.0\gst-plugin-scanner.exe")
foreach ($p in $plugins) { $queue.Enqueue("$bundle\lib\gstreamer-1.0\$p.dll") }
$seen = @{}
while ($queue.Count -gt 0) {
    $file = $queue.Dequeue()
    $deps = & $dumpbin.FullName /nologo /dependents $file |
        Select-String -Pattern '^\s+(\S+\.dll)\s*$' | ForEach-Object { $_.Matches[0].Groups[1].Value }
    foreach ($dll in $deps) {
        $key = $dll.ToLower()
        if ($seen[$key]) { continue }
        $seen[$key] = $true
        $src = Join-Path "$gst\bin" $dll
        if (Test-Path $src) {
            Copy-Item $src "$bundle\bin\"
            $queue.Enqueue("$bundle\bin\$dll")
        }
    }
}

# ---- Microsoft C++ runtime (allowed to ship next to the app) ----
Copy-Item "$($crt.FullName)\*.dll" "$bundle\bin\"

# ---- Licences and readme ----
Copy-Item "$gst\share\licenses\*" "$bundle\licenses\" -Recurse
@"
VTube Collab $version
$('=' * (13 + $version.Length))

Requirements: Windows 10/11, the OBS Spout2 plugin, a VTubing app with Spout2 output turned on.
Nothing else to install. Testers: please read BETA-TESTING.txt.

1. Turn on Spout2 output in your VTubing app.
2. Double-click "Start VTube Collab.bat".
3. Enter your name and pick your avatar from the list.
4. The host clicks "Host a collab...", chooses the room settings, clicks "Open room"
   and shares the room code (e.g. MINTY-OTTER-42). Everyone else clicks "Join a collab...",
   types the code, and waits for the host to click Accept.
5. In OBS, add a Spout2 Capture source for each person: "Collab - <their name>".
   Want everyone in one source instead? Tick "Publish a group source" in the app and
   pick "Collab - Group" (off by default to save resources).

Help > How to use... opens these steps in a separate window you can keep open.

Allow VTube Collab through Windows Firewall when asked. Windows may show "Windows protected
your PC" (the app isn't signed yet): click "More info" > "Run anyway".

Problems? In the app: File > Open log folder, and send the newest .log file.

Command-line version (for testing), in the 'bin' folder:
    collab.exe list
    collab.exe host "Avatar Live" --name Ben [--group]
    collab.exe join "Warudo Camera 1" --name Beat [--group]

Includes GStreamer (LGPL) and Microsoft C++ runtime libraries; licences in the 'licenses' folder.
"@ | Set-Content "$bundle\README.txt" -Encoding UTF8

# Tester guide (Markdown is readable as plain text).
Copy-Item "$root\docs\BETA-TESTING.md" "$bundle\BETA-TESTING.txt"

# Double-click starter next to the readme (the .exe must stay in bin\ with its DLLs).
"@echo off`r`nstart `"`" `"%~dp0bin\VTubeCollab.exe`"`r`n" | Set-Content "$bundle\Start VTube Collab.bat" -Encoding ASCII -NoNewline

Compress-Archive -Path $bundle -DestinationPath $zip

# ---- Report ----
$files = Get-ChildItem $bundle -Recurse -File
"Bundle: $bundle"
"  {0} files, {1:N1} MB unpacked" -f $files.Count, (($files | Measure-Object Length -Sum).Sum / 1MB)
"  bin: {0} DLLs | plugins: {1}" -f (Get-ChildItem "$bundle\bin" -Filter *.dll).Count, (Get-ChildItem "$bundle\lib\gstreamer-1.0").Count
"Zip:    $zip ({0:N1} MB)" -f ((Get-Item $zip).Length / 1MB)
