# Builds a self-contained VTube Collab folder + zip that runs WITHOUT GStreamer installed.
#
#   powershell -ExecutionPolicy Bypass -File tools\make-bundle.ps1
#
# Output: dist\VTubeCollab\  and  dist\VTubeCollab-test.zip
#
# Layout (GStreamer finds its plugins relative to its own DLLs, so this must be kept):
#   VTubeCollab\
#     bin\collab.exe + GStreamer core DLLs + Microsoft C++ runtime DLLs
#     lib\gstreamer-1.0\        only the plugins collab uses
#     libexec\gstreamer-1.0\    gst-plugin-scanner.exe (GStreamer's plugin checker)
#     licenses\                 GStreamer's licence texts (required by the LGPL)
#     README.txt

$ErrorActionPreference = 'Stop'
$root = (Resolve-Path "$PSScriptRoot\..").Path
$dist = Join-Path $root 'dist'
$bundle = Join-Path $dist 'VTubeCollab'
$zip = Join-Path $dist 'VTubeCollab-test.zip'

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
Write-Host "Building collab (release)..."
Push-Location $root
cargo build --release -p collab
if ($LASTEXITCODE -ne 0) { Pop-Location; throw "build failed" }
Pop-Location

# ---- Fresh output folder ----
if (Test-Path $bundle) { Remove-Item $bundle -Recurse -Force }
if (Test-Path $zip) { Remove-Item $zip -Force }
foreach ($d in 'bin', 'lib\gstreamer-1.0', 'libexec\gstreamer-1.0', 'licenses') {
    New-Item -ItemType Directory -Force (Join-Path $bundle $d) | Out-Null
}

Copy-Item "$root\target\release\collab.exe" "$bundle\bin\"
Copy-Item "$gst\libexec\gstreamer-1.0\gst-plugin-scanner.exe" "$bundle\libexec\gstreamer-1.0\"
foreach ($p in $plugins) { Copy-Item "$gst\lib\gstreamer-1.0\$p.dll" "$bundle\lib\gstreamer-1.0\" }

# ---- Copy every GStreamer DLL these files need, and what those need, and so on ----
# `dumpbin /dependents` lists the DLLs a file loads. DLLs not in GStreamer's bin folder
# are part of Windows (or the C++ runtime, handled below) and are skipped.
$queue = New-Object System.Collections.Queue
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
VTube Collab - test build
=========================

Requirements: Windows 10/11, the OBS Spout2 plugin, a VTubing app with Spout2 output turned on.
Nothing else to install.

Open a terminal in the 'bin' folder (Shift + right-click > Open in Terminal), then:

  Start a collab (host):
    .\collab.exe host "<your Spout sender name>" --name <your name>
    -> copy the INVITE code to your partner, paste their REPLY code back.

  Join a collab:
    .\collab.exe join "<your Spout sender name>" --name <your name>
    -> paste the INVITE code, send the REPLY code back.

Your partner appears in OBS as Spout2 source "Collab - <their name>" (Spout2 Capture).
Press Enter to stop. Allow collab.exe through Windows Firewall when asked.

Includes GStreamer (LGPL) and Microsoft C++ runtime libraries; licences in the 'licenses' folder.
"@ | Set-Content "$bundle\README.txt" -Encoding UTF8

Compress-Archive -Path $bundle -DestinationPath $zip

# ---- Report ----
$files = Get-ChildItem $bundle -Recurse -File
"Bundle: $bundle"
"  {0} files, {1:N1} MB unpacked" -f $files.Count, (($files | Measure-Object Length -Sum).Sum / 1MB)
"  bin: {0} DLLs | plugins: {1}" -f (Get-ChildItem "$bundle\bin" -Filter *.dll).Count, (Get-ChildItem "$bundle\lib\gstreamer-1.0").Count
"Zip:    $zip ({0:N1} MB)" -f ((Get-Item $zip).Length / 1MB)
