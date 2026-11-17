//! Picking a hardware H.264 encoder for this PC.
//!
//! GStreamer only registers a hardware encoder if the matching graphics card is
//! present, so "does the element exist?" also means "can this PC use it?".
//! We try them in order of preference and use the first one found.

use gstreamer as gst;

pub struct Encoder {
    pub label: &'static str,
    /// Pipeline text placed between `appsrc` (RGBA frames) and `h264parse`.
    pub pipeline: String,
}

/// `bitrate_kbps` = target bitrate, `gop` = frames between keyframes.
pub fn pick(bitrate_kbps: u32, gop: u64) -> Result<Encoder, String> {
    // (element, label, pipeline text). NVIDIA takes RGBA directly (converts on the GPU);
    // the others want NV12, so `videoconvert` converts first (on the CPU, multi-threaded).
    let candidates = [
        (
            "nvh264enc",
            "NVIDIA (NVENC)",
            format!(
                "nvh264enc name=enc preset=p1 tune=ultra-low-latency zerolatency=true \
                 rc-mode=cbr bitrate={bitrate_kbps} gop-size={gop} bframes=0"
            ),
        ),
        (
            "amfh264enc",
            "AMD (AMF)",
            format!(
                "videoconvert n-threads=0 ! video/x-raw,format=NV12 \
                 ! amfh264enc name=enc usage=ultra-low-latency preset=speed rate-control=cbr \
                 bitrate={bitrate_kbps} gop-size={gop} b-frames=0"
            ),
        ),
        (
            "mfh264enc",
            "Windows Media Foundation",
            format!(
                "videoconvert n-threads=0 ! video/x-raw,format=NV12 \
                 ! mfh264enc name=enc low-latency=true rc-mode=cbr quality-vs-speed=0 \
                 bitrate={bitrate_kbps} gop-size={gop}"
            ),
        ),
    ];

    // For testing: VTC_ENCODER=amfh264enc (etc.) forces a specific encoder.
    let forced = std::env::var("VTC_ENCODER").ok();

    for (element, label, pipeline) in candidates {
        if let Some(f) = &forced {
            if f != element {
                continue;
            }
        }
        if gst::ElementFactory::find(element).is_some() {
            return Ok(Encoder { label, pipeline });
        }
    }
    Err("no hardware H.264 encoder found (needs an NVIDIA, AMD or Intel graphics driver)".into())
}
